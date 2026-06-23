use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use bitcoin::secp256k1::rand::{rngs::OsRng, RngCore};

use super::messages::{RpcAuthEnvelope, RpcMsgReq};
use crate::maker::error::MakerError;

/// Filename for the maker RPC authentication cookie in the data directory.
pub const RPC_COOKIE_FILENAME: &str = "rpc_cookie";

const RPC_COOKIE_TMP_FILENAME: &str = "rpc_cookie.tmp";
const RPC_COOKIE_BYTES: usize = 32;

/// Returns the path to the RPC cookie file for a maker data directory.
pub fn cookie_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RPC_COOKIE_FILENAME)
}

fn cookie_tmp_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RPC_COOKIE_TMP_FILENAME)
}

fn write_cookie_tmp(path: &Path, contents: &[u8]) -> Result<(), MakerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(MakerError::IO)?;
        file.write_all(contents).map_err(MakerError::IO)?;
        file.sync_all().map_err(MakerError::IO)?;
    }

    #[cfg(not(unix))]
    {
        fs::write(path, contents).map_err(MakerError::IO)?;
    }

    Ok(())
}

/// Generates a fresh RPC cookie and writes it to the maker data directory.
///
/// Called when the RPC server starts. Overwrites any existing file (Bitcoin Core `.cookie` pattern).
pub fn write_rpc_cookie(data_dir: &Path) -> Result<String, MakerError> {
    fs::create_dir_all(data_dir)?;

    let mut bytes = [0u8; RPC_COOKIE_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let token = bytes_to_hex(&bytes);

    let tmp_path = cookie_tmp_path(data_dir);
    let final_path = cookie_path(data_dir);

    // Atomic flush: write to tmp file with restrictive mode, then rename over original.
    let write_result = write_cookie_tmp(&tmp_path, token.as_bytes())
        .and_then(|()| fs::rename(&tmp_path, &final_path).map_err(MakerError::IO));

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result.map(|()| token)
}

/// Loads the RPC cookie from the maker data directory (for RPC clients such as `maker-cli`).
pub fn load_rpc_cookie(data_dir: &Path) -> Result<String, MakerError> {
    let path = cookie_path(data_dir);
    let token = fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            std::io::Error::new(
                e.kind(),
                format!(
                    "failed to read {}: is makerd running and is --data-directory correct?",
                    path.display()
                ),
            )
        } else {
            e
        }
    })?;
    Ok(token.trim().to_string())
}

fn bytes_to_hex(bytes: &[u8; RPC_COOKIE_BYTES]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_token_hex(token: &str) -> Result<[u8; RPC_COOKIE_BYTES], MakerError> {
    let hex = token.trim();
    if hex.len() != RPC_COOKIE_BYTES * 2 {
        return Err(MakerError::General("unauthorized"));
    }

    let mut arr = [0u8; RPC_COOKIE_BYTES];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|_| MakerError::General("unauthorized"))?;
        arr[i] = u8::from_str_radix(s, 16).map_err(|_| MakerError::General("unauthorized"))?;
    }
    Ok(arr)
}

fn tokens_equal(provided: &[u8; RPC_COOKIE_BYTES], expected: &[u8; RPC_COOKIE_BYTES]) -> bool {
    let mut diff = 0u8;
    for (a, b) in provided.iter().zip(expected.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Verifies an authenticated RPC envelope and returns the inner request on success.
pub fn verify_rpc_auth(
    envelope: &RpcAuthEnvelope,
    expected_hex: &str,
) -> Result<RpcMsgReq, MakerError> {
    let provided = parse_token_hex(&envelope.token)?;
    let expected = parse_token_hex(expected_hex)?;

    if tokens_equal(&provided, &expected) {
        Ok(envelope.msg.clone())
    } else {
        Err(MakerError::General("unauthorized"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_data_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "coinswap_rpc_cookie_test_{}_{}",
            std::process::id(),
            suffix
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_rpc_cookie_creates_hex_token() {
        let data_dir = temp_data_dir("creates_hex");
        let token = write_rpc_cookie(&data_dir).unwrap();

        assert_eq!(token.len(), RPC_COOKIE_BYTES * 2);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

        let loaded = load_rpc_cookie(&data_dir).unwrap();
        assert_eq!(loaded, token);

        let _ = fs::remove_dir_all(&data_dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_rpc_cookie_sets_restrictive_permissions() {
        let data_dir = temp_data_dir("permissions");
        write_rpc_cookie(&data_dir).unwrap();

        let perms = fs::metadata(cookie_path(&data_dir)).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);

        let _ = fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn write_rpc_cookie_overwrites_previous() {
        let data_dir = temp_data_dir("overwrites");
        let first = write_rpc_cookie(&data_dir).unwrap();
        let second = write_rpc_cookie(&data_dir).unwrap();

        assert_ne!(first, second);
        assert_eq!(load_rpc_cookie(&data_dir).unwrap(), second);

        let _ = fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn verify_rpc_auth_accepts_valid_rejects_invalid() {
        let mut bytes = [0u8; RPC_COOKIE_BYTES];
        bytes[0] = 0xab;
        let token = bytes_to_hex(&bytes);

        let envelope = RpcAuthEnvelope {
            token: token.clone(),
            msg: RpcMsgReq::Ping,
        };

        assert!(matches!(
            verify_rpc_auth(&envelope, &token),
            Ok(RpcMsgReq::Ping)
        ));
        assert!(matches!(
            verify_rpc_auth(&envelope, "00".repeat(RPC_COOKIE_BYTES * 2).as_str()),
            Err(MakerError::General("unauthorized"))
        ));
        assert!(matches!(
            verify_rpc_auth(
                &RpcAuthEnvelope {
                    token: "not-hex".to_string(),
                    msg: RpcMsgReq::Ping,
                },
                &token
            ),
            Err(MakerError::General("unauthorized"))
        ));
    }
}
