use anyhow::Result;
use std::{io::Write, process::Stdio};
use tokio::{io::AsyncWriteExt, process::Command};

pub async fn copy(text: &str) -> Result<()> {
    let commands: Vec<(&str, Vec<&str>)> = if cfg!(windows) {
        vec![("clip.exe", vec![])]
    } else if cfg!(target_os = "macos") {
        vec![("pbcopy", vec![])]
    } else {
        vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ]
    };
    for (program, args) in commands {
        if let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut input) = child.stdin.take() {
                let bytes = if cfg!(windows) {
                    let mut bytes = vec![0xff, 0xfe];
                    for character in text.encode_utf16() {
                        bytes.extend_from_slice(&character.to_le_bytes());
                    }
                    bytes
                } else {
                    text.as_bytes().to_vec()
                };
                let result = input.write_all(&bytes).await;
                drop(input);
                if child.wait().await?.success() && result.is_ok() {
                    return Ok(());
                }
            }
        }
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(alphabet[((bits >> 18) & 63) as usize] as char);
        encoded.push(alphabet[((bits >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            alphabet[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            alphabet[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    let mut stdout = std::io::stdout().lock();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    Ok(())
}
