//! Release signing for the auto-updater (minisign format).
//!
//! - `cargo run -p sign -- keygen <dir>` writes `replay.pub` and `replay.key`
//!   (unencrypted; keep the key out of the repository).
//! - `cargo run -p sign -- sign <file>` signs with the key in the
//!   `MINISIGN_SECRET_KEY` environment variable and writes `<file>.minisig`.

use std::fs;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let dir = Path::new(args.get(1).map(String::as_str).unwrap_or("."));
            fs::create_dir_all(dir).expect("create key folder");
            let pk_path = dir.join("replay.pub");
            let sk_path = dir.join("replay.key");
            if sk_path.exists() {
                eprintln!("{} already exists; not overwriting it", sk_path.display());
                std::process::exit(1);
            }
            let kp = minisign::KeyPair::generate_unencrypted_keypair().expect("generate key pair");
            let pk = kp.pk.to_box().expect("public key").into_string();
            let sk = kp.sk.to_box(Some("Replay release signing key")).expect("secret key").into_string();
            fs::write(&pk_path, &pk).expect("write public key");
            fs::write(&sk_path, &sk).expect("write secret key");
            println!("public key:\n{pk}");
            println!("wrote {} and {}", pk_path.display(), sk_path.display());
        }
        Some("sign") => {
            let file = args.get(1).expect("usage: sign <file>");
            let sk_text = std::env::var("MINISIGN_SECRET_KEY").expect("MINISIGN_SECRET_KEY is not set");
            let sk = minisign::SecretKeyBox::from_string(sk_text.trim())
                .and_then(|b| b.into_unencrypted_secret_key())
                .expect("read secret key");
            let data = fs::File::open(file).expect("open file to sign");
            let name = Path::new(file).file_name().unwrap_or_default().to_string_lossy();
            let sig = minisign::sign(None, &sk, data, Some(&format!("file:{name}")), Some("Replay release signature"))
                .expect("sign");
            fs::write(format!("{file}.minisig"), sig.into_string()).expect("write signature");
            println!("signed {file}");
        }
        _ => {
            eprintln!("usage: sign keygen <dir> | sign sign <file>");
            std::process::exit(2);
        }
    }
}
