//! Writes the output bare repository and handcrafted packfile stream for ordinances.
//!
//! Unlike `compiler-for-precedent`, ordinance paths are deeply nested
//! (`ordinances/{광역}/{기초}/{ordinance_type}/{name}/본문.md` = 6 segments),
//! so this writer keeps a general hierarchical tree cache instead of a
//! hand-tuned 3-level cache. Trees are rebuilt in full on every commit — the
//! per-commit byte cost is small relative to the blob, and every ordinance is
//! committed exactly once (no amendments).

use std::cell::RefCell;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result, anyhow, bail};
use crc32fast::Hasher as Crc32Hasher;
use rustc_hash::FxHashMap as HashMap;
use sha1::{Digest, Sha1};
use smallvec::SmallVec;
use time::{Date, Month, PrimitiveDateTime, Time as CivilTime, UtcOffset};

/// Supported pack entry kinds emitted by the handcrafted writer.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackObjectKind {
    /// Full commit object payload.
    Commit = 1,
    /// Full tree object payload.
    Tree = 2,
    /// Full blob object payload.
    Blob = 3,
}

impl PackObjectKind {
    /// Returns the Git object header name for full objects.
    fn git_type_name(self) -> &'static [u8] {
        match self {
            Self::Commit => b"commit",
            Self::Tree => b"tree",
            Self::Blob => b"blob",
        }
    }
}

/// Git identity pair used in handcrafted commit objects.
#[derive(Debug, Clone, Copy)]
struct GitPerson<'a> {
    /// Display name in the commit header.
    name: &'a str,
    /// Email address in the commit header.
    email: &'a str,
}

/// Author/committer identities paired for one handcrafted commit.
#[derive(Debug, Clone, Copy)]
struct CommitPeople<'a> {
    /// Author identity recorded in the commit body.
    author: GitPerson<'a>,
    /// Committer identity recorded in the commit body.
    committer: GitPerson<'a>,
}

/// Commit timestamp rendered in Korea Standard Time (`+0900`).
#[derive(Debug, Clone, Copy)]
pub struct GitTimestampKst {
    /// Unix timestamp in seconds.
    epoch: i64,
}

impl GitTimestampKst {
    /// Converts a 공포일자 into the deterministic noon-KST commit timestamp.
    ///
    /// Accepts an empty string (returns the Unix epoch) or a bare `YYYYMMDD`
    /// value. Pre-epoch dates are clamped to `1970-01-01` so reruns keep
    /// producing the same commit ids.
    pub fn from_promulgation_date(date: &str) -> Result<Self> {
        if date.is_empty() {
            return Ok(Self { epoch: 0 });
        }
        if date.len() != 8 || !date.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("expected 공포일자 in YYYYMMDD form: {date}");
        }

        let effective = if date < "19700101" {
            String::from("1970-01-01")
        } else {
            format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8])
        };

        let year = effective[0..4].parse::<i32>()?;
        let month = effective[5..7].parse::<u8>()?;
        let day = effective[8..10].parse::<u8>()?;
        let month = Month::try_from(month)?;
        let date = Date::from_calendar_date(year, month, day)?;
        let datetime = PrimitiveDateTime::new(date, CivilTime::from_hms(12, 0, 0)?);
        let offset = UtcOffset::from_hms(9, 0, 0)?;
        Ok(Self {
            epoch: datetime.assume_offset(offset).unix_timestamp(),
        })
    }

    /// Builds a timestamp from a raw Unix epoch (used for seed commits).
    pub const fn from_epoch(epoch: i64) -> Self {
        Self { epoch }
    }
}

/// Owned repository path — either a root-level file or a deep ordinance file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RepoPathBuf {
    /// Root-level repository file such as `README.md`.
    RootFile(String),
    /// Ordinance markdown file at `ordinances/{광역}/{기초}/{type}/{name}/본문.md`.
    Ordinance {
        /// 광역시·도 (or `_미상`).
        gwangyeok: String,
        /// 기초 지자체 (or `_본청`).
        gicho: String,
        /// Normalized ordinance type (조례 / 규칙 / 훈령 / 예규).
        ordinance_type: String,
        /// NFC-normalized 자치법규명.
        name: String,
    },
}

impl RepoPathBuf {
    /// Creates a root-level repository path.
    pub fn root_file(name: impl Into<String>) -> Self {
        Self::RootFile(name.into())
    }

    /// Creates an ordinance markdown path.
    pub fn ordinance_file(
        gwangyeok: impl Into<String>,
        gicho: impl Into<String>,
        ordinance_type: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self::Ordinance {
            gwangyeok: gwangyeok.into(),
            gicho: gicho.into(),
            ordinance_type: ordinance_type.into(),
            name: name.into(),
        }
    }

    /// Returns the sequence of directory components leading to the leaf file.
    fn segments(&self) -> Vec<&str> {
        match self {
            Self::RootFile(name) => vec![name.as_str()],
            Self::Ordinance {
                gwangyeok,
                gicho,
                ordinance_type,
                name,
            } => vec![
                "ordinances",
                gwangyeok.as_str(),
                gicho.as_str(),
                ordinance_type.as_str(),
                name.as_str(),
                "본문.md",
            ],
        }
    }
}

impl fmt::Display for RepoPathBuf {
    /// Renders the repository path in Git's slash-separated form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let segments = self.segments();
        for (i, segment) in segments.iter().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            f.write_str(segment)?;
        }
        Ok(())
    }
}

/// Precomputes the canonical blob id and compressed pack payload for one file body.
pub fn precompute_blob(content: &[u8]) -> ([u8; 20], Vec<u8>) {
    (
        git_hash(PackObjectKind::Blob.git_type_name(), content),
        compress(content),
    )
}

/// In-memory tree node used by the hierarchical tree cache.
#[derive(Debug, Default)]
struct TreeNode {
    /// Child directories keyed by name.
    dirs: HashMap<Vec<u8>, TreeNode>,
    /// Child blobs keyed by name, mapped to object ids.
    files: HashMap<Vec<u8>, [u8; 20]>,
}

/// One entry in the pack index, accumulated during pack writing.
struct IdxEntry {
    /// Object id of the packed object.
    sha: [u8; 20],
    /// CRC-32 of the raw pack entry bytes.
    crc32: u32,
    /// Byte offset of the entry within the pack file.
    offset: u64,
}

/// Low-level writer that streams packfile entries directly to the final `.pack` file.
struct PackWriter {
    /// Buffered writer for the pack file.
    file: BufWriter<File>,
    /// Number of unique objects appended so far.
    object_count: u32,
    /// Filesystem path of the `.pack` file being written.
    path: PathBuf,
    /// Object ids already emitted.
    seen: HashMap<[u8; 20], u64>,
    /// Accumulated index entries.
    idx_entries: Vec<IdxEntry>,
    /// Running byte offset.
    bytes_written: u64,
}

/// Writes the generated ordinance history into a fresh bare Git repository.
pub struct BareRepoWriter {
    /// Streaming pack writer used for all objects in the temporary repo.
    writer: PackWriter,
    /// Temporary bare repository path populated before the final rename.
    temp_output: PathBuf,
    /// Requested output path for the finished bare repository.
    final_output: PathBuf,
    /// Root node of the in-memory tree state.
    root: TreeNode,
    /// Parent commit id for the next handcrafted commit object.
    parent_commit: Option<[u8; 20]>,
}

impl BareRepoWriter {
    /// Creates a new temporary bare repository writer for the requested output path.
    pub fn create(output: &Path) -> Result<Self> {
        let final_output = output.to_path_buf();
        let temp_output = {
            let parent = output.parent().unwrap_or_else(|| Path::new("."));
            let name = output
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow!("invalid output path: {}", output.display()))?;
            parent.join(format!(".{name}.tmp-{}", process::id()))
        };
        if temp_output.exists() {
            remove_path(&temp_output)?;
        }

        let parent = temp_output
            .parent()
            .context("temporary output path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let pack_path = temp_output.join("objects/pack/tmp_pack.pack");
        fs::create_dir_all(
            pack_path
                .parent()
                .context("pack path unexpectedly missing parent")?,
        )?;

        Ok(Self {
            writer: PackWriter::new(&pack_path)?,
            temp_output,
            final_output,
            root: TreeNode::default(),
            parent_commit: None,
        })
    }

    /// Commits one rendered ordinance markdown file using bot authorship.
    pub fn commit_ordinance(
        &mut self,
        path: &RepoPathBuf,
        markdown: &[u8],
        blob_sha: [u8; 20],
        compressed_blob: &[u8],
        message: &str,
        time: GitTimestampKst,
    ) -> Result<()> {
        let bot = GitPerson {
            name: "legalize-kr-bot",
            email: "bot@legalize.kr",
        };
        self.writer.write_precompressed_object(
            PackObjectKind::Blob,
            markdown.len(),
            blob_sha,
            compressed_blob,
        )?;
        self.insert_path(path, blob_sha);
        let root_sha = self.materialize_root()?;
        let commit_sha = self.write_commit(
            root_sha,
            message,
            CommitPeople {
                author: bot,
                committer: bot,
            },
            time,
        )?;
        self.parent_commit = Some(commit_sha);
        Ok(())
    }

    /// Commits a static repository file (used for the seed README commit).
    pub fn commit_static(
        &mut self,
        path: &RepoPathBuf,
        content: &[u8],
        message: &str,
        epoch: i64,
    ) -> Result<()> {
        let author = GitPerson {
            name: "legalize-kr-bot",
            email: "bot@legalize.kr",
        };
        let (blob_sha, compressed_blob) = precompute_blob(content);
        self.writer.write_precompressed_object(
            PackObjectKind::Blob,
            content.len(),
            blob_sha,
            &compressed_blob,
        )?;
        self.insert_path(path, blob_sha);
        let root_sha = self.materialize_root()?;
        let commit_sha = self.write_commit(
            root_sha,
            message,
            CommitPeople {
                author,
                committer: author,
            },
            GitTimestampKst { epoch },
        )?;
        self.parent_commit = Some(commit_sha);
        Ok(())
    }

    /// Finalizes the pack, writes `main`, and moves the temporary repo into place.
    pub fn finish(mut self) -> Result<()> {
        self.writer.finish()?;

        if let Some(parent_commit) = self.parent_commit {
            let refs_heads = self.temp_output.join("refs/heads");
            fs::create_dir_all(&refs_heads)
                .with_context(|| format!("failed to create {}", refs_heads.display()))?;
            fs::write(
                refs_heads.join("main"),
                format!("{}\n", hex(&parent_commit)),
            )
            .with_context(|| format!("failed to write {}", refs_heads.join("main").display()))?;
        }
        fs::write(self.temp_output.join("HEAD"), "ref: refs/heads/main\n").with_context(|| {
            format!(
                "failed to write {}",
                self.temp_output.join("HEAD").display()
            )
        })?;

        if self.final_output.exists() {
            remove_path(&self.final_output)?;
        }
        fs::rename(&self.temp_output, &self.final_output).with_context(|| {
            format!(
                "failed to move {} to {}",
                self.temp_output.display(),
                self.final_output.display()
            )
        })?;
        Ok(())
    }

    /// Inserts a new leaf blob into the in-memory tree at the given repo path.
    fn insert_path(&mut self, path: &RepoPathBuf, blob_sha: [u8; 20]) {
        let segments = path.segments();
        let (leaf, dirs) = segments.split_last().expect("path must have a leaf");
        let mut node = &mut self.root;
        for segment in dirs {
            node = node.dirs.entry(segment.as_bytes().to_vec()).or_default();
        }
        node.files.insert(leaf.as_bytes().to_vec(), blob_sha);
    }

    /// Materializes the root tree (and every dirty subtree) and returns the root sha.
    fn materialize_root(&mut self) -> Result<[u8; 20]> {
        //
        // Rebuild every tree from scratch. Each ordinance commit touches one
        // leaf and its ancestor chain, but since `TreeNode` owns its bytes we
        // still rewrite parent nodes anyway. The write_object call skips
        // duplicates via the `seen` set, so unchanged subtrees cost nothing.
        //
        let root = std::mem::take(&mut self.root);
        let (sha, root) = write_tree(root, &mut self.writer)?;
        self.root = root;
        Ok(sha)
    }

    /// Serializes and appends one commit object to the pack stream.
    fn write_commit(
        &mut self,
        tree: [u8; 20],
        message: &str,
        people: CommitPeople<'_>,
        time: GitTimestampKst,
    ) -> Result<[u8; 20]> {
        use std::fmt::Write as _;
        let mut commit = String::with_capacity(512);
        let tree_hex = hex_buf(&tree);
        let tree_hex_str = std::str::from_utf8(&tree_hex).unwrap();
        writeln!(commit, "tree {tree_hex_str}").unwrap();
        if let Some(parent) = self.parent_commit {
            let parent_hex = hex_buf(&parent);
            let parent_hex_str = std::str::from_utf8(&parent_hex).unwrap();
            writeln!(commit, "parent {parent_hex_str}").unwrap();
        }
        write!(
            commit,
            "author {} <{}> {} +0900\ncommitter {} <{}> {} +0900\n\n{message}",
            people.author.name,
            people.author.email,
            time.epoch,
            people.committer.name,
            people.committer.email,
            time.epoch,
        )
        .unwrap();
        self.writer
            .write_object(PackObjectKind::Commit, commit.as_bytes())
    }
}

/// Writes one tree node (and all of its children) to the pack, returning its sha.
///
/// Takes the node by value and returns it back so the caller can keep reusing
/// the allocated `HashMap`s between commits without reallocating the whole
/// structure. Deep-copies of child SHAs are not needed — the tree stays intact.
fn write_tree(mut node: TreeNode, writer: &mut PackWriter) -> Result<([u8; 20], TreeNode)> {
    //
    // Recursively materialize children first so we can build this tree's bytes.
    // Directories and files share the same namespace in Git tree objects, so we
    // merge them and sort with the directory-name-suffix rule from git.
    //
    let mut child_dirs: Vec<(Vec<u8>, [u8; 20], TreeNode)> = Vec::with_capacity(node.dirs.len());
    for (name, child) in node.dirs.drain() {
        let (sha, returned) = write_tree(child, writer)?;
        child_dirs.push((name, sha, returned));
    }

    //
    // Merge files + dirs into a single entry list and sort using Git's tree
    // ordering rule (trailing `/` for dirs, NUL for files).
    //
    let mut entries: Vec<(bool, Vec<u8>, [u8; 20])> =
        Vec::with_capacity(node.files.len() + child_dirs.len());
    for (name, sha) in node.files.iter() {
        entries.push((false, name.clone(), *sha));
    }
    for (name, sha, _) in &child_dirs {
        entries.push((true, name.clone(), *sha));
    }
    entries.sort_by(|a, b| {
        let common = a.1.len().min(b.1.len());
        match a.1[..common].cmp(&b.1[..common]) {
            std::cmp::Ordering::Equal => {
                let a_tail = if a.0 { b'/' } else { 0 };
                let b_tail = if b.0 { b'/' } else { 0 };
                let a_next = a.1.get(common).copied().unwrap_or(a_tail);
                let b_next = b.1.get(common).copied().unwrap_or(b_tail);
                a_next.cmp(&b_next)
            }
            other => other,
        }
    });

    let mut body: Vec<u8> = Vec::with_capacity(entries.len() * 32);
    for (is_tree, name, sha) in entries {
        body.extend_from_slice(if is_tree { b"40000 " } else { b"100644 " });
        body.extend_from_slice(&name);
        body.push(0);
        body.extend_from_slice(&sha);
    }

    let sha = writer.write_object(PackObjectKind::Tree, &body)?;

    //
    // Put the child dirs back so the parent keeps ownership for the next commit.
    //
    for (name, _, child) in child_dirs {
        node.dirs.insert(name, child);
    }
    Ok((sha, node))
}

impl PackWriter {
    /// Creates a new pack writer that writes directly to the final `.pack` file.
    fn new(path: &Path) -> Result<Self> {
        let mut file = BufWriter::with_capacity(4 << 20, File::create(path)?);
        let pack_header: [u8; 12] = [b'P', b'A', b'C', b'K', 0, 0, 0, 2, 0, 0, 0, 0];
        file.write_all(&pack_header)?;
        Ok(Self {
            file,
            object_count: 0,
            path: path.to_path_buf(),
            seen: HashMap::default(),
            idx_entries: Vec::new(),
            bytes_written: 12,
        })
    }

    /// Appends one full object to the pack unless it was already emitted.
    fn write_object(&mut self, object_type: PackObjectKind, data: &[u8]) -> Result<[u8; 20]> {
        let sha = git_hash(object_type.git_type_name(), data);
        self.write_precompressed_object(object_type, data.len(), sha, &compress(data))
    }

    /// Appends one full object whose id and compressed payload were prepared earlier.
    fn write_precompressed_object(
        &mut self,
        object_type: PackObjectKind,
        size: usize,
        sha: [u8; 20],
        compressed: &[u8],
    ) -> Result<[u8; 20]> {
        if self.seen.contains_key(&sha) {
            return Ok(sha);
        }

        let offset = self.bytes_written;
        self.seen.insert(sha, offset);
        let header_bytes = encode_pack_entry_header(object_type, size);

        let mut crc = Crc32Hasher::new();
        crc.update(&header_bytes);
        crc.update(compressed);

        self.file.write_all(&header_bytes)?;
        self.file.write_all(compressed)?;
        self.bytes_written += header_bytes.len() as u64 + compressed.len() as u64;
        self.object_count += 1;
        self.idx_entries.push(IdxEntry {
            sha,
            crc32: crc.finalize(),
            offset,
        });
        Ok(sha)
    }

    /// Finalizes the pack file and generates the `.idx`.
    fn finish(&mut self) -> Result<()> {
        self.file.flush()?;

        let inner = self.file.get_mut();
        inner.seek(SeekFrom::Start(8))?;
        inner.write_all(&self.object_count.to_be_bytes())?;
        inner.flush()?;

        let mut reader = BufReader::with_capacity(4 << 20, File::open(&self.path)?);
        let mut hasher = Sha1::new();
        let mut buffer = [0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        drop(reader);
        let pack_checksum: [u8; 20] = hasher.finalize().into();

        let mut pack_file = fs::OpenOptions::new().append(true).open(&self.path)?;
        pack_file.write_all(&pack_checksum)?;
        pack_file.flush()?;
        drop(pack_file);

        self.write_idx_v2(&pack_checksum)?;

        let checksum_hex = hex(&pack_checksum);
        let pack_dir = self.path.parent().context("pack path has no parent")?;
        let final_pack = pack_dir.join(format!("pack-{checksum_hex}.pack"));
        let final_idx = pack_dir.join(format!("pack-{checksum_hex}.idx"));
        let tmp_idx = self.path.with_extension("idx");
        fs::rename(&self.path, &final_pack)?;
        fs::rename(&tmp_idx, &final_idx)?;
        Ok(())
    }

    /// Writes the `.idx` v2 index file alongside the pack.
    fn write_idx_v2(&mut self, pack_checksum: &[u8; 20]) -> Result<()> {
        self.idx_entries.sort_unstable_by(|a, b| a.sha.cmp(&b.sha));

        let idx_path = self.path.with_extension("idx");
        let mut f = BufWriter::with_capacity(4 << 20, File::create(&idx_path)?);
        let mut hasher = Sha1::new();

        let mut write = |data: &[u8]| -> Result<()> {
            f.write_all(data)?;
            hasher.update(data);
            Ok(())
        };

        write(&[0xff, 0x74, 0x4f, 0x63])?;
        write(&[0x00, 0x00, 0x00, 0x02])?;

        let mut fanout = [0u32; 256];
        for entry in &self.idx_entries {
            fanout[entry.sha[0] as usize] += 1;
        }
        for i in 1..256 {
            fanout[i] += fanout[i - 1];
        }
        for count in &fanout {
            write(&count.to_be_bytes())?;
        }

        for entry in &self.idx_entries {
            write(&entry.sha)?;
        }

        for entry in &self.idx_entries {
            write(&entry.crc32.to_be_bytes())?;
        }

        let mut large_offsets = Vec::new();
        for entry in &self.idx_entries {
            if entry.offset >= 0x8000_0000 {
                let large_idx = large_offsets.len() as u32;
                write(&(large_idx | 0x8000_0000).to_be_bytes())?;
                large_offsets.push(entry.offset);
            } else {
                write(&(entry.offset as u32).to_be_bytes())?;
            }
        }

        for &off in &large_offsets {
            write(&off.to_be_bytes())?;
        }

        write(pack_checksum)?;

        f.flush()?;
        let idx_checksum: [u8; 20] = hasher.finalize().into();
        f.write_all(&idx_checksum)?;
        f.flush()?;

        Ok(())
    }
}

/// Encodes the variable-length PACK entry header into a stack buffer.
#[inline]
fn encode_pack_entry_header(object_type: PackObjectKind, size: usize) -> SmallVec<[u8; 16]> {
    let mut buf = SmallVec::new();
    let mut header = ((object_type as u8 & 0b111) << 4) | (size as u8 & 0x0f);
    let mut remaining = size >> 4;
    if remaining > 0 {
        header |= 0x80;
    }
    buf.push(header);
    while remaining > 0 {
        let mut byte = (remaining & 0x7f) as u8;
        remaining >>= 7;
        if remaining > 0 {
            byte |= 0x80;
        }
        buf.push(byte);
    }
    buf
}

/// Deletes a file or directory tree at `path`.
fn remove_path(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to read {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

thread_local! {
    /// Reusable scratch buffer for compression output to avoid per-call allocation.
    static COMP_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };

    /// Reuses one fast zlib compressor per thread for whole-buffer pack payload compression.
    #[cfg(feature = "default")]
    static COMPRESSOR: RefCell<libdeflater::Compressor> =
        RefCell::new(libdeflater::Compressor::new(libdeflater::CompressionLvl::new(1).unwrap()));
}

/// Compresses one pack payload with the current fast zlib setting.
fn compress(data: &[u8]) -> Vec<u8> {
    COMP_BUF.with(|buf_cell| {
        #[cfg(feature = "default")]
        return COMPRESSOR.with(|comp_cell| {
            let mut comp = comp_cell.borrow_mut();
            let mut buf = buf_cell.borrow_mut();
            let bound = comp.zlib_compress_bound(data.len());
            buf.resize(bound, 0);
            let actual = comp
                .zlib_compress(data, &mut buf)
                .expect("zlib_compress_bound() must allocate enough space");
            buf[..actual].to_vec()
        });

        #[cfg(not(feature = "default"))]
        {
            use zlib_rs::{DeflateConfig, ReturnCode, compress_bound, compress_slice};

            let mut buf = buf_cell.borrow_mut();
            buf.resize(compress_bound(data.len()), 0);
            let (compressed, rc) = compress_slice(&mut buf, data, DeflateConfig::new(1));
            assert_eq!(rc, ReturnCode::Ok);
            compressed.to_vec()
        }
    })
}

/// Computes the canonical Git object id for one unhashed object body.
fn git_hash(type_name: &[u8], data: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    let mut len_buf = [0_u8; 20];
    let mut cursor = len_buf.len();
    let mut value = data.len();
    loop {
        cursor -= 1;
        len_buf[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }

    hasher.update(type_name);
    hasher.update(b" ");
    hasher.update(&len_buf[cursor..]);
    hasher.update([0]);
    hasher.update(data);
    hasher.finalize().into()
}

/// Stack-based hex encoding for the commit write hot path (no heap allocation).
fn hex_buf(sha: &[u8; 20]) -> [u8; 40] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 40];
    for (i, &b) in sha.iter().enumerate() {
        buf[i * 2] = HEX[(b >> 4) as usize];
        buf[i * 2 + 1] = HEX[(b & 0xf) as usize];
    }
    buf
}

/// Hex-encodes one object id for refs, logging, and non-hot-path usage.
fn hex(sha: &[u8; 20]) -> String {
    let buf = hex_buf(sha);
    String::from_utf8(buf.to_vec()).expect("hex digits are valid UTF-8")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::{Command, Output};

    use tempfile::TempDir;

    use super::*;

    /// Creates a Git command with user config disabled for deterministic behavior.
    fn git_command() -> Command {
        let mut command = Command::new("git");
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_CONFIG_NOSYSTEM", "1");
        command.env_remove("GIT_DIR");
        command.env_remove("GIT_WORK_TREE");
        command
    }

    /// Converts a failed Git subprocess result into a rich error.
    fn ensure_command_success(output: Output, context: &str) -> Result<()> {
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        bail!(
            "{context}: exit status {}\nstderr: {stderr}\nstdout: {stdout}",
            output.status
        )
    }

    fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
        let mut command = git_command();
        command.arg("-C").arg(repo);
        for arg in args {
            command.arg(arg);
        }
        let output = command.output().unwrap();
        let stdout = output.stdout.clone();
        ensure_command_success(output, "git test helper").unwrap();
        String::from_utf8(stdout).unwrap()
    }

    fn output_repo(temp: &TempDir) -> PathBuf {
        temp.path().join("output.git")
    }

    #[test]
    fn clamps_pre_epoch_dates() {
        let ts = GitTimestampKst::from_promulgation_date("19491021").unwrap();
        assert_eq!(ts.epoch, 10800);
    }

    #[test]
    fn empty_date_uses_epoch() {
        let ts = GitTimestampKst::from_promulgation_date("").unwrap();
        assert_eq!(ts.epoch, 0);
    }

    #[test]
    fn rejects_non_compact_dates() {
        let error = GitTimestampKst::from_promulgation_date("2024-01-01").unwrap_err();
        assert!(error.to_string().contains("YYYYMMDD"));
    }

    #[test]
    fn builds_deep_tree_layout() {
        let temp = TempDir::new().unwrap();
        let output = output_repo(&temp);
        let mut writer = BareRepoWriter::create(&output).unwrap();

        writer
            .commit_static(
                &RepoPathBuf::root_file("README.md"),
                b"hello\n",
                "initial commit",
                0,
            )
            .unwrap();

        let (blob, compressed) = precompute_blob(b"body\n");
        writer
            .commit_ordinance(
                &RepoPathBuf::ordinance_file("서울특별시", "_본청", "조례", "샘플 조례"),
                b"body\n",
                blob,
                &compressed,
                "조례: 샘플 조례\n\n자치법규ID: 1",
                GitTimestampKst::from_promulgation_date("20240101").unwrap(),
            )
            .unwrap();

        writer.finish().unwrap();

        let commits = git_stdout(&output, ["rev-list", "--count", "HEAD"]);
        assert_eq!(commits.trim(), "2");

        let ls = git_stdout(
            &output,
            [
                "-c",
                "core.quotePath=false",
                "ls-tree",
                "-r",
                "--name-only",
                "HEAD",
            ],
        );
        assert!(ls.contains("README.md"));
        assert!(ls.contains("ordinances/서울특별시/_본청/조례/샘플 조례/본문.md"));

        let body = git_stdout(
            &output,
            [
                "show",
                "HEAD:ordinances/서울특별시/_본청/조례/샘플 조례/본문.md",
            ],
        );
        assert_eq!(body, "body\n");
    }
}
