//! User-defined gene signatures, shared by all sections, persisted as one JSON file
//! (`<state>/signatures.json`, rewritten atomically on every change).

use crate::cache::write_atomic;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAX_SIGNATURES: usize = 1000;
pub const MAX_GENES: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Raw entries as UCell takes them: `GENE`, `GENE+` (positive) or `GENE-` (negative).
    pub genes: Vec<String>,
    pub created_unix: u64,
    pub updated_unix: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignatureInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub genes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    signatures: Vec<Signature>,
}

pub struct SignatureStore {
    path: PathBuf,
    sigs: Mutex<Vec<Signature>>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(format!(
        "sig_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}

pub fn is_signature_id(s: &str) -> bool {
    s.len() == 16
        && s.starts_with("sig_")
        && s[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Trim and check a signature; returns the cleaned input.
pub fn validate(mut s: SignatureInput) -> Result<SignatureInput> {
    s.name = s.name.trim().to_string();
    s.description = s.description.trim().to_string();
    if s.name.is_empty() || s.name.chars().count() > 80 || s.name.chars().any(|c| c.is_control()) {
        bail!("signature name must be 1-80 characters without control characters");
    }
    if s.description.chars().count() > 2000 {
        bail!("description is limited to 2000 characters");
    }
    s.genes = s
        .genes
        .iter()
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty())
        .collect();
    if s.genes.is_empty() || s.genes.len() > MAX_GENES {
        bail!("a signature needs 1-{MAX_GENES} genes");
    }
    for g in &s.genes {
        let core = g
            .strip_suffix('+')
            .or_else(|| g.strip_suffix('-'))
            .unwrap_or(g);
        if core.is_empty()
            || g.chars().count() > 80
            || g.chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | ';' | '"' | '\''))
        {
            bail!("invalid gene entry {g:?} (use the gene name, optionally followed by + or -)");
        }
    }
    Ok(s)
}

impl SignatureStore {
    pub fn open(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join("signatures.json");
        let sigs = match std::fs::read(&path) {
            Ok(b) => {
                let f: File =
                    serde_json::from_slice(&b).map_err(|e| anyhow!("{}: {e}", path.display()))?;
                if f.version != 1 {
                    bail!("{}: unsupported version {}", path.display(), f.version);
                }
                f.signatures
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(anyhow!("{}: {e}", path.display())),
        };
        Ok(SignatureStore {
            path,
            sigs: Mutex::new(sigs),
        })
    }

    fn save(&self, sigs: &[Signature]) -> Result<()> {
        let f = File {
            version: 1,
            signatures: sigs.to_vec(),
        };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&f)?)
    }

    pub fn list(&self) -> Vec<Signature> {
        self.sigs.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Signature> {
        self.sigs
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    pub fn create(&self, input: SignatureInput) -> Result<Signature> {
        let input = validate(input)?;
        let mut sigs = self.sigs.lock().unwrap();
        if sigs.len() >= MAX_SIGNATURES {
            bail!("at most {MAX_SIGNATURES} signatures");
        }
        if sigs.iter().any(|s| s.name == input.name) {
            bail!("a signature named {:?} already exists", input.name);
        }
        let t = now();
        let s = Signature {
            id: new_id()?,
            name: input.name,
            description: input.description,
            genes: input.genes,
            created_unix: t,
            updated_unix: t,
        };
        let mut next = sigs.clone();
        next.push(s.clone());
        self.save(&next)?;
        *sigs = next;
        Ok(s)
    }

    pub fn update(&self, id: &str, input: SignatureInput) -> Result<Option<Signature>> {
        let input = validate(input)?;
        let mut sigs = self.sigs.lock().unwrap();
        let Some(i) = sigs.iter().position(|s| s.id == id) else {
            return Ok(None);
        };
        if sigs.iter().any(|s| s.id != id && s.name == input.name) {
            bail!("a signature named {:?} already exists", input.name);
        }
        let mut next = sigs.clone();
        let s = &mut next[i];
        s.name = input.name;
        s.description = input.description;
        s.genes = input.genes;
        s.updated_unix = now();
        let out = s.clone();
        self.save(&next)?;
        *sigs = next;
        Ok(Some(out))
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let mut sigs = self.sigs.lock().unwrap();
        let next: Vec<Signature> = sigs.iter().filter(|s| s.id != id).cloned().collect();
        if next.len() == sigs.len() {
            return Ok(false);
        }
        self.save(&next)?;
        *sigs = next;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, genes: &[&str]) -> SignatureInput {
        SignatureInput {
            name: name.into(),
            description: String::new(),
            genes: genes.iter().map(|g| g.to_string()).collect(),
        }
    }

    #[test]
    fn crud_persists_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let st = SignatureStore::open(dir.path()).unwrap();
        let a = st
            .create(input(" T cells ", &["CD3E", " CD2 ", "CD19-", ""]))
            .unwrap();
        assert_eq!(a.name, "T cells");
        assert_eq!(a.genes, vec!["CD3E", "CD2", "CD19-"]);
        assert!(is_signature_id(&a.id));
        assert!(
            st.create(input("T cells", &["X"])).is_err(),
            "duplicate name"
        );
        assert!(
            st.create(input("bad", &["CD3E CD2"])).is_err(),
            "whitespace in a gene"
        );
        assert!(st.create(input("bad", &["-"])).is_err(), "suffix only");
        assert!(st.create(input("", &["A"])).is_err());
        let u = st.update(&a.id, input("T", &["CD3D"])).unwrap().unwrap();
        assert_eq!(u.genes, vec!["CD3D"]);
        // Persisted: a new store sees the update.
        let again = SignatureStore::open(dir.path()).unwrap();
        assert_eq!(again.get(&a.id).unwrap().name, "T");
        assert!(again.delete(&a.id).unwrap());
        assert!(!again.delete(&a.id).unwrap());
        assert!(SignatureStore::open(dir.path()).unwrap().list().is_empty());
    }
}
