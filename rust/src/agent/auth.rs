//! One secret token per role, stored in `agent_tokens.json` (owner-only).
//!
//! The server derives the role **from the token**, so a client cannot claim
//! a role it holds no token for.  Tokens are printed on demand by
//! `dos-commander agent-token <role>` and handed to the agent client's MCP
//! configuration (`DOS_AGENT_TOKEN`).

use std::collections::HashMap;
#[cfg_attr(not(unix), allow(unused_imports))]
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::Role;

pub const TOKEN_FILE: &str = "agent_tokens.json";

pub struct Tokens {
    by_role: HashMap<Role, String>,
}

pub fn token_path(dir: &Path) -> PathBuf {
    dir.join(TOKEN_FILE)
}

/// 32 random bytes as hex.  Reads the OS entropy source directly; there is
/// no weaker fallback — failing is better than a guessable token.
#[cfg(unix)]
fn random_token() -> io::Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The connector needs Unix sockets (`server::start`), so there are no tokens
/// to make elsewhere; refusing here beats a confusing "path not found".
#[cfg(not(unix))]
fn random_token() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the agent connector needs Unix sockets",
    ))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

/// Create the agent directory and make it owner-only.  Refuses (error) when
/// the directory cannot be secured, e.g. it belongs to someone else: a
/// directory others can write to would let them plant a symlink where the
/// token file is written.
#[cfg(unix)]
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "cannot make {} owner-only (is it yours?): {e}",
                dir.display()
            ),
        )
    })
}

#[cfg(not(unix))]
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Tighten an existing file to owner-only (unix).
#[cfg(unix)]
fn secure_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

impl Tokens {
    /// Load the token file, creating it (and any missing role's token).
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        secure_dir(dir)?;
        let path = token_path(dir);
        let mut by_name: HashMap<String, String> = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        let mut changed = false;
        for r in Role::ALL {
            let ok = by_name.get(r.name()).is_some_and(|t| t.len() >= 32);
            if !ok {
                by_name.insert(r.name().to_string(), random_token()?);
                changed = true;
            }
        }
        if changed {
            let text = serde_json::to_string_pretty(&by_name).map_err(io::Error::other)?;
            write_private(&path, text.as_bytes())?;
        }
        if path.exists() {
            secure_file(&path)?;
        }
        let by_role = Role::ALL
            .into_iter()
            .filter_map(|r| by_name.get(r.name()).map(|t| (r, t.clone())))
            .collect();
        Ok(Self { by_role })
    }

    pub fn token_for(&self, role: Role) -> Option<&str> {
        self.by_role.get(&role).map(String::as_str)
    }

    /// The role that owns `token`, compared without early exit.
    pub fn role_for(&self, token: &str) -> Option<Role> {
        let mut found = None;
        for (role, t) in &self.by_role {
            if constant_time_eq(t.as_bytes(), token.as_bytes()) {
                found = Some(*role);
            }
        }
        found
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dos-agent-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[cfg(unix)]
    #[test]
    fn creates_distinct_tokens_that_identify_their_role() {
        let dir = temp_dir("tok");
        let t = Tokens::load_or_create(&dir).unwrap();
        let mut seen = std::collections::HashSet::new();
        for r in Role::ALL {
            let tok = t.token_for(r).unwrap();
            assert_eq!(tok.len(), 64);
            assert!(seen.insert(tok.to_string()), "tokens must differ");
            assert_eq!(t.role_for(tok), Some(r));
        }
        assert_eq!(t.role_for("nope"), None);
        assert_eq!(t.role_for(""), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn tokens_persist_and_file_is_owner_only() {
        let dir = temp_dir("persist");
        let a = Tokens::load_or_create(&dir).unwrap();
        let b = Tokens::load_or_create(&dir).unwrap();
        for r in Role::ALL {
            assert_eq!(
                a.token_for(r),
                b.token_for(r),
                "second load must not rotate"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_loose_directory_and_token_file_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("loose");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let first = Tokens::load_or_create(&dir).unwrap();
        std::fs::set_permissions(token_path(&dir), std::fs::Permissions::from_mode(0o644)).unwrap();
        let again = Tokens::load_or_create(&dir).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "directory is owner-only");
        assert_eq!(
            mode(&token_path(&dir)),
            0o600,
            "token file is owner-only again"
        );
        assert_eq!(
            first.token_for(Role::Risk),
            again.token_for(Role::Risk),
            "tightening does not rotate tokens"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn short_or_corrupt_entries_are_replaced() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(token_path(&dir), r#"{"analyst":"short"}"#).unwrap();
        let t = Tokens::load_or_create(&dir).unwrap();
        assert_ne!(t.token_for(Role::Analyst), Some("short"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn constant_time_eq_rejects_length_and_content_differences() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
