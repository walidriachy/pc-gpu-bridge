//! Path mapping between the Mac and `PCBridge\workspace\` on the PC.
//!
//! `/Users/me/Videos/trip/a.mp4`  <->  `D:\PCBridge\workspace\Videos\trip\a.mp4`
//! Root-relative form (what the agent reports): `workspace/Videos/trip/a.mp4`

use crate::config::SharedFolder;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PathMapper {
    pub folders: Vec<SharedFolder>,
    pub pc_root: String,
}

/// Lexically normalise a path (resolve `.` and `..`) without touching the disk.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl PathMapper {
    pub fn new(folders: Vec<SharedFolder>, pc_root: impl Into<String>) -> Self {
        Self { folders, pc_root: pc_root.into() }
    }

    /// Mac absolute path -> root-relative posix path (`workspace/Name/...`), if inside a shared folder.
    pub fn mac_to_rel(&self, mac: &Path) -> Option<String> {
        let mac = normalize(mac);
        // Longest matching shared folder wins (handles nested shares).
        let mut best: Option<(&SharedFolder, usize)> = None;
        for f in &self.folders {
            let base = normalize(Path::new(&f.mac_path));
            if mac.starts_with(&base) {
                let len = base.components().count();
                if best.map(|(_, l)| len > l).unwrap_or(true) {
                    best = Some((f, len));
                }
            }
        }
        let (f, _) = best?;
        let rest = mac.strip_prefix(normalize(Path::new(&f.mac_path))).ok()?;
        let mut rel = format!("workspace/{}", f.name);
        for c in rest.components() {
            rel.push('/');
            rel.push_str(&c.as_os_str().to_string_lossy());
        }
        Some(rel)
    }

    /// Root-relative posix path -> absolute Windows path on the PC.
    pub fn rel_to_pc(&self, rel: &str) -> String {
        let root = self.pc_root.trim_end_matches(['\\', '/']);
        if self.posix_root() {
            // Agent running on Linux/WSL2 (or a test agent on macOS).
            return format!("{root}/{rel}");
        }
        format!("{}\\{}", root, rel.replace('/', "\\"))
    }

    fn posix_root(&self) -> bool {
        self.pc_root.starts_with('/')
    }

    pub fn mac_to_pc(&self, mac: &Path) -> Option<String> {
        self.mac_to_rel(mac).map(|r| self.rel_to_pc(&r))
    }

    /// Root-relative posix path -> Mac absolute path.
    pub fn rel_to_mac(&self, rel: &str) -> Option<PathBuf> {
        let rel = rel.replace('\\', "/");
        let mut parts = rel.split('/');
        if parts.next()? != "workspace" {
            return None;
        }
        let name = parts.next()?;
        let f = self.folders.iter().find(|f| f.name == name)?;
        // Mac paths are always POSIX, whatever OS this code is compiled for.
        let mut p = f.mac_path.trim_end_matches('/').to_string();
        for part in parts.filter(|s| !s.is_empty()) {
            p.push('/');
            p.push_str(part);
        }
        Some(PathBuf::from(p))
    }

    /// Absolute PC path (`D:\PCBridge\workspace\...`) -> Mac path.
    pub fn pc_to_mac(&self, pc: &str) -> Option<PathBuf> {
        if self.posix_root() {
            let root = self.pc_root.trim_end_matches('/');
            return pc.strip_prefix(root).and_then(|r| self.rel_to_mac(r.trim_start_matches('/')));
        }
        let root = self.pc_root.trim_end_matches(['\\', '/']).to_lowercase().replace('/', "\\");
        let norm = pc.replace('/', "\\");
        if !norm.to_lowercase().starts_with(&root) {
            return None;
        }
        let rel = norm[root.len()..].trim_start_matches('\\').replace('\\', "/");
        self.rel_to_mac(&rel)
    }

    /// Replace any PC paths inside text (e.g. tool output) with Mac paths.
    pub fn rewrite_output(&self, text: &str) -> String {
        let root = self.pc_root.trim_end_matches(['\\', '/']);
        if root.is_empty() || !text.contains(root) {
            return text.to_string();
        }
        let re = regex::Regex::new(&format!(r"{}[\\/][^\s'\x22]+", regex::escape(root))).unwrap();
        re.replace_all(text, |c: &regex::Captures| {
            self.pc_to_mac(&c[0]).map(|p| p.display().to_string()).unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
    }

    /// Default name for a newly shared folder: its last component, de-duplicated.
    pub fn suggest_name(&self, mac_path: &Path) -> String {
        let base = mac_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Shared".into())
            .replace(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == ' '), "_");
        let mut name = base.clone();
        let mut i = 2;
        while self.folders.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
            name = format!("{base}-{i}");
            i += 1;
        }
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> PathMapper {
        PathMapper::new(
            vec![
                SharedFolder { mac_path: "/Users/w/Videos".into(), name: "Videos".into() },
                SharedFolder { mac_path: "/Users/w/Code/app".into(), name: "app".into() },
                SharedFolder { mac_path: "/Users/w/Code/app/assets".into(), name: "assets".into() },
            ],
            r"D:\PCBridge",
        )
    }

    #[test]
    fn mac_to_pc_basic() {
        let m = mapper();
        assert_eq!(
            m.mac_to_pc(Path::new("/Users/w/Videos/trip/a b.mp4")).unwrap(),
            r"D:\PCBridge\workspace\Videos\trip\a b.mp4"
        );
        assert_eq!(m.mac_to_rel(Path::new("/Users/w/Videos")).unwrap(), "workspace/Videos");
        assert!(m.mac_to_pc(Path::new("/Users/w/Desktop/x.mp4")).is_none());
        assert!(m.mac_to_pc(Path::new("/Users/w/VideosExtra/x.mp4")).is_none());
    }

    #[test]
    fn dotdot_cannot_escape() {
        let m = mapper();
        assert!(m.mac_to_rel(Path::new("/Users/w/Videos/../Desktop/x")).is_none());
        assert_eq!(m.mac_to_rel(Path::new("/Users/w/Videos/a/../b.mp4")).unwrap(), "workspace/Videos/b.mp4");
    }

    #[test]
    fn nested_share_longest_wins() {
        let m = mapper();
        assert_eq!(m.mac_to_rel(Path::new("/Users/w/Code/app/assets/i.png")).unwrap(), "workspace/assets/i.png");
        assert_eq!(m.mac_to_rel(Path::new("/Users/w/Code/app/src/i.ts")).unwrap(), "workspace/app/src/i.ts");
    }

    #[test]
    fn reverse_mapping() {
        let m = mapper();
        assert_eq!(m.rel_to_mac("workspace/Videos/out/x.mp4").unwrap(), PathBuf::from("/Users/w/Videos/out/x.mp4"));
        assert_eq!(m.pc_to_mac(r"d:\pcbridge\workspace\Videos\x.mp4").unwrap(), PathBuf::from("/Users/w/Videos/x.mp4"));
        assert!(m.rel_to_mac("jobs/2026/x").is_none());
        assert!(m.rel_to_mac("workspace/Unknown/x").is_none());
        assert!(m.pc_to_mac(r"C:\Windows\x").is_none());
    }

    #[test]
    fn roundtrip_property() {
        let m = mapper();
        for p in ["/Users/w/Videos/a.mp4", "/Users/w/Code/app/src/deep/x.ts", "/Users/w/Videos/ü/ñ.mov"] {
            let pc = m.mac_to_pc(Path::new(p)).unwrap();
            assert_eq!(m.pc_to_mac(&pc).unwrap(), PathBuf::from(p));
        }
    }

    #[test]
    fn posix_agent_root() {
        let m = PathMapper::new(
            vec![SharedFolder { mac_path: "/Users/w/V".into(), name: "V".into() }],
            "/home/me/PCBridge/",
        );
        assert_eq!(m.mac_to_pc(Path::new("/Users/w/V/a.mp4")).unwrap(), "/home/me/PCBridge/workspace/V/a.mp4");
        assert_eq!(m.pc_to_mac("/home/me/PCBridge/workspace/V/a.mp4").unwrap(), PathBuf::from("/Users/w/V/a.mp4"));
    }

    #[test]
    fn rewrite_output_text() {
        let m = mapper();
        let out = m.rewrite_output(r"Output #0, mp4, to 'D:\PCBridge\workspace\Videos\out.mp4':");
        assert!(out.contains("/Users/w/Videos/out.mp4"), "{out}");
        assert_eq!(m.rewrite_output("nothing here"), "nothing here");
    }

    #[test]
    fn suggest_unique_name() {
        let m = mapper();
        assert_eq!(m.suggest_name(Path::new("/Volumes/X/Videos")), "Videos-2");
        assert_eq!(m.suggest_name(Path::new("/Users/w/Renders")), "Renders");
    }
}
