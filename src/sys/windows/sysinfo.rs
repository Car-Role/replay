//! System details for bug reports: Windows, the GPU and its encoders, and
//! the audio devices with their formats (a Bluetooth headset in hands-free
//! mode turns all its sound, game included, into 16 kHz mono).

use windows::Win32::Media::Audio::{EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications, eConsole, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::{HSTRING, PCWSTR, w};

use crate::config::Codec;

/// Windows, GPU and audio device lines. `mic` is the chosen microphone's
/// endpoint id (empty for the default).
pub fn system_report(mic: &str) -> String {
    let mut out = format!("Windows: {}\n", windows_version());
    out += &format!("Screen capture: {}\n", if super::capture::is_supported() { "supported" } else { "NOT supported" });
    match super::gpu::Gpu::create() {
        Ok(g) => {
            out += &format!("GPU: {} (vendor {:04x})\n", g.name, g.vendor);
            for c in Codec::ALL {
                let names = super::encoder::find_encoders(c, Some(&g)).map(|l| l.into_iter().map(|(_, n)| n).collect::<Vec<_>>()).unwrap_or_default();
                out += &format!("  {} encoders: {}\n", c.label(), if names.is_empty() { "none".to_string() } else { names.join(", ") });
            }
        }
        Err(e) => out += &format!("GPU: error {e}\n"),
    }
    out += &format!("Recorder running: {}\n", if super::shell::recorder_running() { "yes" } else { "no" });
    out += &format!("Output (default): {}\n", default_device(eRender, eConsole));
    out += &format!("Output (communications): {}\n", default_device(eRender, eCommunications));
    out += &format!("Microphone (default communications): {}\n", default_device(eCapture, eCommunications));
    if !mic.is_empty() {
        out += &format!("Microphone (chosen in Replay): {}\n", device_by_id(mic));
    }
    out
}

fn reg_string(name: PCWSTR) -> Option<String> {
    let key = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    ok.is_ok().then(|| crate::util::from_wide(&buf))
}

fn reg_dword(name: PCWSTR) -> Option<u32> {
    let key = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let mut v = 0u32;
    let mut size = 4u32;
    let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_DWORD, None, Some((&raw mut v).cast()), Some(&mut size)) };
    ok.is_ok().then_some(v)
}

/// "Windows 11 24H2 (build 26100.2894)".
fn windows_version() -> String {
    let build: u32 = reg_string(w!("CurrentBuild")).and_then(|b| b.parse().ok()).unwrap_or(0);
    let ubr = reg_dword(w!("UBR")).unwrap_or(0);
    let product = reg_string(w!("ProductName")).unwrap_or_else(|| "Windows".into());
    // Windows 11 still reports "Windows 10" as its product name.
    let product = if build >= 22000 { product.replace("Windows 10", "Windows 11") } else { product };
    let release = reg_string(w!("DisplayVersion")).unwrap_or_default();
    format!("{product} {release} (build {build}.{ubr})")
}

fn enumerator() -> Option<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() }
}

fn default_device(flow: EDataFlow, role: ERole) -> String {
    let Some(en) = enumerator() else { return "unavailable".into() };
    match unsafe { en.GetDefaultAudioEndpoint(flow, role) } {
        Ok(d) => describe(&d),
        Err(_) => "none".into(),
    }
}

fn device_by_id(id: &str) -> String {
    let Some(en) = enumerator() else { return "unavailable".into() };
    match unsafe { en.GetDevice(&HSTRING::from(id)) } {
        Ok(d) => describe(&d),
        Err(_) => "not found (unplugged?)".into(),
    }
}

/// "Headphones (WH-1000XM4), 48000 Hz, 2 ch, 32-bit", with a warning for
/// hands-free (telephone quality) endpoints.
fn describe(dev: &IMMDevice) -> String {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::IAudioClient;
    let name = unsafe { dev.OpenPropertyStore(STGM_READ).and_then(|ps| ps.GetValue(&PKEY_Device_FriendlyName)).map(|v| v.to_string()).unwrap_or_else(|_| "unknown".into()) };
    let format = unsafe {
        dev.Activate::<IAudioClient>(CLSCTX_ALL, None).and_then(|c| c.GetMixFormat()).map(|f| {
            let (rate, ch, bits) = ((*f).nSamplesPerSec, (*f).nChannels, (*f).wBitsPerSample);
            CoTaskMemFree(Some(f as _));
            (rate, ch, bits)
        })
    };
    match format {
        Ok((rate, ch, bits)) => {
            let hands_free = rate <= 16_000 || name.to_ascii_lowercase().contains("hands-free") || name.to_ascii_lowercase().contains("hands free");
            let warn = if hands_free { "  << Bluetooth hands-free: low quality for all sound while its mic is open" } else { "" };
            format!("{name}, {rate} Hz, {ch} ch, {bits}-bit{warn}")
        }
        Err(_) => format!("{name}, format unknown"),
    }
}
