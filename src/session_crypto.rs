use anyhow::{Context, Result, anyhow, bail, ensure};
use argon2::Argon2;
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::Sha256;
use std::{collections::HashMap, sync::Mutex};

const ENC_PREFIX: &str = "{\"_jaa_enc\":1,\"p\":\"";
const ENC_SUFFIX: &str = "\"}";
const DOMAIN_MASK: [u8; 32] = [
    0x7a, 0x3e, 0x91, 0xc4, 0x18, 0x55, 0xe2, 0x0d, 0x6f, 0xb3, 0x4c, 0x89, 0x2a, 0xd7, 0x61, 0xf0,
    0x33, 0x9c, 0x14, 0xa8, 0x5b, 0x72, 0xe9, 0x06, 0x41, 0xcf, 0x83, 0x2d, 0x96, 0x50, 0xbe, 0x17,
];

#[derive(Default)]
pub struct KeyVault {
    half: Mutex<HashMap<String, [u8; 32]>>,
    full: Mutex<HashMap<String, [u8; 32]>>,
}

impl KeyVault {
    pub fn get_half(&self, id: &str) -> Option<[u8; 32]> {
        self.half.lock().ok().and_then(|g| g.get(id).copied())
    }
    pub fn set_half(&self, id: &str, key: [u8; 32]) {
        if let Ok(mut g) = self.half.lock() {
            g.insert(id.to_owned(), key);
        }
    }
    pub fn get_full(&self, id: &str) -> Option<[u8; 32]> {
        self.full.lock().ok().and_then(|g| g.get(id).copied())
    }
    pub fn set_full(&self, id: &str, key: [u8; 32]) {
        if let Ok(mut g) = self.full.lock() {
            g.insert(id.to_owned(), key);
        }
    }
    pub fn lock_full(&self, id: &str) {
        if let Ok(mut g) = self.full.lock() {
            g.remove(id);
        }
    }
    pub fn remove_all(&self, id: &str) {
        if let Ok(mut g) = self.half.lock() {
            g.remove(id);
        }
        if let Ok(mut g) = self.full.lock() {
            g.remove(id);
        }
    }
}

pub fn migrate(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS sys_cache_meta(k TEXT PRIMARY KEY, v BLOB NOT NULL);",
    )?;
    Ok(())
}

pub fn is_encrypted(text: &str) -> bool {
    text.starts_with(ENC_PREFIX) && text.ends_with(ENC_SUFFIX)
}

pub fn encrypt_text(key: &[u8; 32], session_id: &str, plaintext: &str) -> Result<String> {
    let unbound =
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("Invalid AES-256-GCM key"))?;
    let aead = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut in_out = plaintext.as_bytes().to_vec();
    aead.seal_in_place_append_tag(nonce, Aad::from(session_id.as_bytes()), &mut in_out)
        .map_err(|_| anyhow!("AES-256-GCM encryption failed"))?;
    let mut combined = Vec::with_capacity(12 + in_out.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&in_out);
    Ok(format!(
        "{ENC_PREFIX}{}{ENC_SUFFIX}",
        base64::engine::general_purpose::STANDARD.encode(combined)
    ))
}

pub fn decrypt_text(key: &[u8; 32], session_id: &str, payload: &str) -> Result<String> {
    let Some(rest) = payload.strip_prefix(ENC_PREFIX) else {
        return Ok(payload.to_owned());
    };
    let Some(encoded) = rest.strip_suffix(ENC_SUFFIX) else {
        return Ok(payload.to_owned());
    };
    let combined = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("Invalid encrypted payload encoding")?;
    ensure!(combined.len() >= 12 + 16, "Truncated encrypted payload");
    let (nonce_bytes, cipher) = combined.split_at(12);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes)
        .map_err(|_| anyhow!("Invalid AES-256-GCM nonce"))?;
    let unbound =
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("Invalid AES-256-GCM key"))?;
    let aead = LessSafeKey::new(unbound);
    let mut buf = cipher.to_vec();
    let plain = aead
        .open_in_place(nonce, Aad::from(session_id.as_bytes()), &mut buf)
        .map_err(|_| anyhow!("AES-256-GCM decryption failed"))?;
    String::from_utf8(plain.to_vec()).context("Decrypted payload is not valid UTF-8")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key");
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut res = [0u8; 32];
    res.copy_from_slice(&out);
    res
}

fn hw_fingerprint() -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(b"jaa:hw-root:v1:");
    for path in [
        "/etc/machine-id",
        "/var/lib/dbus/machine-id",
        "/sys/class/dmi/id/product_uuid",
        "/sys/class/tpm/tpm0/tpm_version_major",
    ] {
        if let Ok(bytes) = std::fs::read(path)
            && !bytes.is_empty()
        {
            hasher.update(path.as_bytes());
            hasher.update(&bytes);
        }
    }
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn try_tpm2_seal(secret: &[u8; 32]) -> Option<Vec<u8>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if !std::path::Path::new("/dev/tpmrm0").exists() {
        return None;
    }
    let mut child = Command::new("systemd-creds")
        .args(["encrypt", "--with-key=tpm2", "--name=jaa-root", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(secret);
    }
    let out = child.wait_with_output().ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        Some(out.stdout)
    } else {
        None
    }
}

fn try_tpm2_unseal(blob: &[u8]) -> Option<[u8; 32]> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("systemd-creds")
        .args(["decrypt", "--name=jaa-root", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(blob);
    }
    let out = child.wait_with_output().ok()?;
    if out.status.success() && out.stdout.len() == 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out.stdout);
        Some(arr)
    } else {
        None
    }
}

fn load_or_init_root(db: &Connection) -> Result<[u8; 32]> {
    let shard_a: [u8; 32] = if let Some(b64) = db
        .query_row(
            "SELECT value FROM meta WHERE key='_sys_schema_sig_v2'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&b64)
        && bytes.len() == 32
    {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        arr
    } else {
        let mut arr = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut arr);
        let b64 = base64::engine::general_purpose::STANDARD.encode(arr);
        db.execute(
            "INSERT INTO meta(key,value) VALUES('_sys_schema_sig_v2',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [b64],
        )?;
        arr
    };

    let shard_b: [u8; 32] = if let Some(blob) = db
        .query_row(
            "SELECT v FROM sys_cache_meta WHERE k='wal_chk_0'",
            [],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()?
    {
        if blob.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&blob);
            arr
        } else if let Some(unsealed) = try_tpm2_unseal(&blob) {
            unsealed
        } else {
            bail!("Failed to unseal hardware-bound root key shard");
        }
    } else {
        let mut arr = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut arr);
        let stored = try_tpm2_seal(&arr).unwrap_or_else(|| arr.to_vec());
        db.execute(
            "INSERT INTO sys_cache_meta(k,v) VALUES('wal_chk_0',?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![stored],
        )?;
        arr
    };

    let hw = hw_fingerprint();
    let mut root = [0u8; 32];
    for i in 0..32 {
        root[i] = shard_a[i] ^ shard_b[i] ^ DOMAIN_MASK[i] ^ hw[i];
    }
    Ok(root)
}

fn session_lookup_key(root: &[u8; 32], session_id: &str) -> String {
    let tag = hmac_sha256(root, format!("slot:{session_id}").as_bytes());
    format!(
        "chk_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&tag[..16])
    )
}

pub fn ensure_half_key(db: &Connection, vault: &KeyVault, session_id: &str) -> Result<[u8; 32]> {
    if let Some(cached) = vault.get_half(session_id) {
        return Ok(cached);
    }
    let root = load_or_init_root(db)?;
    let slot = session_lookup_key(&root, session_id);
    let salt: [u8; 16] = if let Some(blob) = db
        .query_row(
            "SELECT v FROM sys_cache_meta WHERE k=?1",
            [&slot],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()?
        && blob.len() == 16
    {
        let mut s = [0u8; 16];
        s.copy_from_slice(&blob);
        s
    } else {
        let mut s = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut s);
        db.execute(
            "INSERT INTO sys_cache_meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![slot, s.as_slice()],
        )?;
        s
    };
    let mut msg = Vec::with_capacity(32 + session_id.len() + 16);
    msg.extend_from_slice(b"jaa:half:v1:");
    msg.extend_from_slice(session_id.as_bytes());
    msg.extend_from_slice(&salt);
    let key = hmac_sha256(&root, &msg);
    vault.set_half(session_id, key);
    Ok(key)
}

pub fn remove_half_slot(db: &Connection, session_id: &str) -> Result<()> {
    let root = load_or_init_root(db)?;
    let slot = session_lookup_key(&root, session_id);
    db.execute("DELETE FROM sys_cache_meta WHERE k=?1", [slot])?;
    Ok(())
}

pub fn derive_full_key(password: &str, salt: &[u8; 16]) -> Result<[u8; 32]> {
    ensure!(!password.is_empty(), "Session password cannot be empty");
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("Key derivation failed: {e}"))?;
    Ok(key)
}

pub fn create_full_meta(password: &str, session_id: &str) -> Result<(String, [u8; 32])> {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let key = derive_full_key(password, &salt)?;
    let verifier = encrypt_text(&key, session_id, &format!("JAA_FULL_VERIFY:{session_id}"))?;
    let meta = serde_json::json!({
        "v": 1,
        "s": base64::engine::general_purpose::STANDARD.encode(salt),
        "t": verifier,
    })
    .to_string();
    Ok((meta, key))
}

pub fn verify_full_password(meta_json: &str, session_id: &str, password: &str) -> Result<[u8; 32]> {
    let meta: Value =
        serde_json::from_str(meta_json).context("Invalid session encryption metadata")?;
    let salt_b64 = meta["s"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing encryption salt"))?;
    let salt_vec = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .context("Invalid encryption salt")?;
    ensure!(salt_vec.len() == 16, "Invalid encryption salt length");
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&salt_vec);
    let key = derive_full_key(password, &salt)?;
    let verifier = meta["t"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing encryption verifier"))?;
    let plain = decrypt_text(&key, session_id, verifier)
        .map_err(|_| anyhow!("INVALID_SESSION_PASSWORD: Incorrect session password"))?;
    if plain != format!("JAA_FULL_VERIFY:{session_id}") {
        bail!("INVALID_SESSION_PASSWORD: Incorrect session password");
    }
    Ok(key)
}
