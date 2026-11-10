//! Path-based operations on a Volume (the way the kernel layer drives it) and a model to compare against.
#![allow(dead_code)]

use std::collections::BTreeMap;

use lucina_fat::{BlockIo, DirEntry, Error, Node, Result, Volume};

pub struct Tree<D: BlockIo> {
    pub vol: Volume<D>,
}

fn split(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

impl<D: BlockIo> Tree<D> {
    pub fn new(vol: Volume<D>) -> Self {
        Tree { vol }
    }

    pub fn resolve(&mut self, path: &str) -> Result<Node> {
        let mut n = self.vol.root();
        for c in split(path) {
            n = self.vol.lookup(&n, c)?;
        }
        Ok(n)
    }

    fn parent_and_name<'a>(&mut self, path: &'a str) -> Result<(Node, &'a str)> {
        let parts = split(path);
        let (last, dirs) = parts.split_last().ok_or(Error::Invalid)?;
        let mut n = self.vol.root();
        for c in dirs {
            n = self.vol.lookup(&n, c)?;
        }
        Ok((n, last))
    }

    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        let (mut p, name) = self.parent_and_name(path)?;
        self.vol.create(&mut p, name, true)?;
        self.vol.sync_node(&mut p)
    }

    pub fn create(&mut self, path: &str) -> Result<Node> {
        let (mut p, name) = self.parent_and_name(path)?;
        let n = self.vol.create(&mut p, name, false)?;
        self.vol.sync_node(&mut p)?;
        Ok(n)
    }

    pub fn write_file(&mut self, path: &str, off: u64, data: &[u8]) -> Result<usize> {
        let mut n = match self.resolve(path) {
            Ok(n) => n,
            Err(Error::NotFound) => self.create(path)?,
            Err(e) => return Err(e),
        };
        let w = self.vol.write(&mut n, off, data)?;
        self.vol.sync_node(&mut n)?;
        Ok(w)
    }

    pub fn truncate(&mut self, path: &str, size: u64) -> Result<()> {
        let mut n = self.resolve(path)?;
        self.vol.truncate(&mut n, size)?;
        self.vol.sync_node(&mut n)
    }

    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let mut n = self.resolve(path)?;
        let mut out = vec![0u8; n.size as usize];
        let mut done = 0;
        while done < out.len() {
            // Read in odd-sized pieces to exercise unaligned reads.
            let end = (done + 3001).min(out.len());
            let r = self.vol.read(&mut n, done as u64, &mut out[done..end])?;
            assert!(r > 0);
            done += r;
        }
        Ok(out)
    }

    pub fn unlink(&mut self, path: &str) -> Result<()> {
        let (mut p, name) = self.parent_and_name(path)?;
        let mut n = self.vol.lookup(&p, name)?;
        if n.is_dir {
            return Err(Error::IsDir);
        }
        self.vol.remove_entry(&mut p, &mut n)?;
        self.vol.sync_node(&mut p)?;
        self.vol.release(&mut n)
    }

    pub fn rmdir(&mut self, path: &str) -> Result<()> {
        let (mut p, name) = self.parent_and_name(path)?;
        let mut n = self.vol.lookup(&p, name)?;
        if !n.is_dir {
            return Err(Error::NotDir);
        }
        self.vol.remove_entry(&mut p, &mut n)?;
        self.vol.sync_node(&mut p)?;
        self.vol.release(&mut n)
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let (mut op, oname) = self.parent_and_name(from)?;
        let (mut np, nname) = self.parent_and_name(to)?;
        let mut node = self.vol.lookup(&op, oname)?;
        let mut target = self.vol.lookup(&np, nname).ok();
        let same = op.dir_id() == np.dir_id();
        let res = if same {
            self.vol
                .rename(&mut op, &mut node, None, nname, target.as_mut())
        } else {
            self.vol
                .rename(&mut op, &mut node, Some(&mut np), nname, target.as_mut())
        };
        res?;
        self.vol.sync_node(&mut op)?;
        if !same {
            self.vol.sync_node(&mut np)?;
        }
        self.vol.sync_node(&mut node)?;
        if let Some(mut t) = target
            && t.pos.is_none()
        {
            self.vol.release(&mut t)?;
        }
        Ok(())
    }

    pub fn list(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let d = self.resolve(path)?;
        let mut v = Vec::new();
        self.vol.read_dir(&d, 0, &mut |e| {
            v.push(e.clone());
            true
        })?;
        Ok(v)
    }

    pub fn names(&mut self, path: &str) -> Result<Vec<String>> {
        let mut v: Vec<String> = self.list(path)?.into_iter().map(|e| e.name).collect();
        v.sort();
        Ok(v)
    }

    pub fn sync(&mut self) -> Result<()> {
        self.vol.sync()
    }
}

type Entry = (String, Option<Vec<u8>>);

/// The expected content of a tree: folded path -> (display name, Some(content) for files / None for dirs).
#[derive(Default, Clone)]
pub struct Model {
    pub entries: BTreeMap<String, Entry>,
}

pub fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}

impl Model {
    pub fn key(path: &str) -> String {
        split(path)
            .iter()
            .map(|c| fold(c))
            .collect::<Vec<_>>()
            .join("/")
    }
    pub fn has(&self, path: &str) -> bool {
        path.trim_matches('/').is_empty() || self.entries.contains_key(&Self::key(path))
    }
    pub fn is_dir(&self, path: &str) -> bool {
        path.trim_matches('/').is_empty()
            || matches!(self.entries.get(&Self::key(path)), Some((_, None)))
    }
    pub fn add_dir(&mut self, path: &str) {
        let name = split(path).last().unwrap().to_string();
        self.entries.insert(Self::key(path), (name, None));
    }
    pub fn add_file(&mut self, path: &str, data: Vec<u8>) {
        let name = split(path).last().unwrap().to_string();
        self.entries.insert(Self::key(path), (name, Some(data)));
    }
    pub fn file_mut(&mut self, path: &str) -> &mut Vec<u8> {
        self.entries
            .get_mut(&Self::key(path))
            .unwrap()
            .1
            .as_mut()
            .unwrap()
    }
    pub fn children(&self, dir: &str) -> Vec<String> {
        let prefix = Self::key(dir);
        let depth = split(dir).len() + 1;
        self.entries
            .iter()
            .filter(|(k, _)| {
                let parts: Vec<&str> = k.split('/').collect();
                parts.len() == depth && (prefix.is_empty() || k.starts_with(&format!("{prefix}/")))
            })
            .map(|(_, (n, _))| n.clone())
            .collect()
    }
    /// Moves an entry and everything below it.
    pub fn rename(&mut self, from: &str, to: &str) {
        let fk = Self::key(from);
        let tk = Self::key(to);
        let new_name = split(to).last().unwrap().to_string();
        let moved: Vec<(String, Entry)> = self
            .entries
            .iter()
            .filter(|(k, _)| **k == fk || k.starts_with(&format!("{fk}/")))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, _) in &moved {
            self.entries.remove(k);
        }
        // A replaced target goes away (files only here).
        self.entries.remove(&tk);
        for (k, (name, content)) in moved {
            let nk = format!("{tk}{}", &k[fk.len()..]);
            let name = if k == fk { new_name.clone() } else { name };
            self.entries.insert(nk, (name, content));
        }
    }
    pub fn remove(&mut self, path: &str) {
        self.entries.remove(&Self::key(path));
    }
}

/// Checks a tree against the model: names (case preserved), kinds and file contents.
pub fn assert_matches<D: BlockIo>(t: &mut Tree<D>, m: &Model, dir: &str) {
    let mut want = m.children(dir);
    want.sort();
    let got = t.names(dir).unwrap();
    assert_eq!(got, want, "listing of {dir:?}");
    for name in want {
        let path = format!("{dir}/{name}");
        let (_, content) = &m.entries[&Model::key(&path)];
        match content {
            None => assert_matches(t, m, &path),
            Some(data) => {
                let got = t.read_file(&path).unwrap();
                assert_eq!(got.len(), data.len(), "size of {path}");
                assert!(got == *data, "content of {path} differs");
            }
        }
    }
}
