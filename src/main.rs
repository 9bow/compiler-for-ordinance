//! Builds a fresh bare Git repository from cached ELIS ordinance XML files.
//!
//! The compiler reads an existing `.cache/ordinance/` tree in two passes:
//! metadata is collected and sorted by `(공포일자 ASC, 자치법규ID ASC)` first,
//! then each XML document is fully parsed, rendered to Markdown, and committed
//! into a new bare repo.

/// Writes the output bare repository and handcrafted packfile stream.
mod git_repo;
/// Jurisdiction name/type classification helpers.
mod jurisdictions;
/// Renders parsed ordinance data into Markdown and commit messages.
mod render;
/// Parses cached XML documents into metadata and body structures.
mod xml_parser;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rayon::prelude::*;

use crate::git_repo::{BareRepoWriter, GitTimestampKst, RepoPathBuf, precompute_blob};
use crate::render::{build_commit_message, compute_path, ordinance_to_markdown};
use crate::xml_parser::{
    OrdinanceDetail, OrdinanceMetadata, parse_metadata_only, parse_ordinance_body,
};

/// Bundled README payload for the synthetic initial commit.
const REPOSITORY_README: &[u8] = include_bytes!("../assets/README.md");

/// Global allocator tuned for high-throughput allocation-heavy pack generation.
#[cfg(feature = "default")]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Command-line interface for one-shot cache compilation.
#[derive(Debug, Parser)]
#[command(name = "compiler-for-ordinance")]
#[command(about = "Compile cached ELIS ordinance XML into a fresh bare Git repository")]
struct Cli {
    /// Path to the existing `.cache/ordinance/` directory.
    cache_dir: PathBuf,

    /// Output bare repository path.
    #[arg(short = 'o', long = "output", default_value = "output.git")]
    output: PathBuf,
}

/// Pass-1 planning record for one XML document.
#[derive(Debug, Clone)]
struct PlannedEntry {
    /// Cache filename stem (ordinance id used for file lookup).
    stem: String,
    /// Final repository path assigned during planning.
    path: RepoPathBuf,
    /// Metadata collected during the cheap planning pass.
    metadata: OrdinanceMetadata,
}

/// Fully rendered pass-2 output that is ready to commit.
struct Rendered {
    /// Destination repository path for the Markdown file.
    path: RepoPathBuf,
    /// Final Markdown bytes stored in Git.
    markdown: Vec<u8>,
    /// Canonical Git blob id for the rendered Markdown.
    blob_sha: [u8; 20],
    /// Precompressed PACK payload for the rendered Markdown blob.
    compressed_blob: Vec<u8>,
    /// Commit message for this revision.
    message: String,
    /// Deterministic KST commit timestamp derived during pass 2.
    time: GitTimestampKst,
}

/// Number of entries rendered per worker batch before the writer catches up.
const CHUNK_SIZE: usize = 500;

/// Synthetic initial commit epoch (Unix 0, mirrors laws compiler).
const INITIAL_COMMIT_EPOCH: i64 = 0;

/// Parses CLI flags and runs the compiler.
fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

/// Executes the full two-pass cache-to-Git compilation pipeline.
fn run(cli: Cli) -> Result<()> {
    let cache_dir = cli.cache_dir.clone();
    if !cache_dir.is_dir() {
        anyhow::bail!("cache directory not found: {}", cache_dir.display());
    }

    eprintln!("1. Scanning cache metadata");
    let entries = {
        let files = read_sorted_files(&cache_dir, "xml")?;
        let parsed = files
            .par_iter()
            .map(|path| -> Result<Option<PlannedEntry>> {
                let stem = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
                    .with_context(|| format!("invalid file name: {}", path.display()))?;
                let xml =
                    fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
                match parse_metadata_only(&xml) {
                    Ok(Some(metadata)) => {
                        let Some(repo_path) = compute_path(&metadata) else {
                            eprintln!(
                                "warning: skipping ordinance with missing type/name: {}",
                                path.display()
                            );
                            return Ok(None);
                        };
                        Ok(Some(PlannedEntry {
                            stem,
                            path: repo_path,
                            metadata,
                        }))
                    }
                    Ok(None) => {
                        eprintln!("warning: skipping non-ordinance XML {}", path.display());
                        Ok(None)
                    }
                    Err(error) => {
                        eprintln!(
                            "warning: skipping unparsable cache file {}: {error:#}",
                            path.display()
                        );
                        Ok(None)
                    }
                }
            })
            .collect::<Vec<_>>();

        let mut entries = Vec::with_capacity(files.len());
        for planned in parsed {
            if let Some(planned) = planned? {
                entries.push(planned);
            }
        }

        //
        // Sort by (공포일자 ASC, int(자치법규ID) ASC) so commit history reads
        // oldest-first regardless of cache filename order. Non-numeric ids sort
        // lexicographically after numeric ones to keep ordering total.
        //
        entries.sort_by(|left, right| {
            let date_cmp = left
                .metadata
                .promulgation_date
                .cmp(&right.metadata.promulgation_date);
            if date_cmp != std::cmp::Ordering::Equal {
                return date_cmp;
            }
            let left_id = left.metadata.ordinance_id.parse::<u64>().ok();
            let right_id = right.metadata.ordinance_id.parse::<u64>().ok();
            match (left_id, right_id) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.metadata.ordinance_id.cmp(&right.metadata.ordinance_id),
            }
        });

        entries
    };
    if entries.is_empty() {
        anyhow::bail!(
            "no valid ordinance XML files found under {}",
            cache_dir.display()
        );
    }

    eprintln!(
        "2. Writing {} commits to {}",
        entries.len(),
        cli.output.display()
    );
    let mut repo = BareRepoWriter::create(&cli.output)?;
    repo.commit_static(
        &RepoPathBuf::root_file("README.md"),
        REPOSITORY_README,
        "initial commit",
        INITIAL_COMMIT_EPOCH,
    )?;
    eprintln!("  committed README.md");

    let total = entries.len();
    let chunks: Vec<&[PlannedEntry]> = entries.chunks(CHUNK_SIZE).collect();
    let mut pending: Option<Vec<Result<Rendered>>> = None;
    let mut committed = 0usize;

    for (index, chunk) in chunks.iter().enumerate() {
        let cache_dir_for_thread = cache_dir.clone();
        let next = if index + 1 < chunks.len() {
            let next_chunk: Vec<PlannedEntry> = chunks[index + 1].to_vec();
            let next_cache_dir = cache_dir_for_thread.clone();
            Some(std::thread::spawn(move || {
                next_chunk
                    .par_iter()
                    .map(|entry| render_entry(&next_cache_dir, entry))
                    .collect::<Vec<_>>()
            }))
        } else {
            None
        };

        let rendered = if let Some(previous) = pending.take() {
            previous
        } else {
            chunk
                .par_iter()
                .map(|entry| render_entry(&cache_dir, entry))
                .collect::<Vec<_>>()
        };

        for rendered in rendered {
            let rendered = rendered?;
            repo.commit_ordinance(
                &rendered.path,
                &rendered.markdown,
                rendered.blob_sha,
                &rendered.compressed_blob,
                &rendered.message,
                rendered.time,
            )?;
            committed += 1;
            if committed.is_multiple_of(500) || committed == total {
                eprintln!("  committed {committed}/{total}");
            }
        }

        if let Some(handle) = next {
            pending = Some(handle.join().expect("render worker panicked"));
        }
    }

    eprintln!("3. Finalizing pack + index");
    repo.finish()?;
    Ok(())
}

/// Parses, renders, and packages one planned XML entry for pass 2.
fn render_entry(cache_dir: &Path, entry: &PlannedEntry) -> Result<Rendered> {
    let xml_path = cache_dir.join(format!("{}.xml", entry.stem));
    let xml =
        fs::read(&xml_path).with_context(|| format!("failed to read {}", xml_path.display()))?;
    let body = parse_ordinance_body(&xml)
        .with_context(|| format!("failed to parse {}", xml_path.display()))?;
    let detail = OrdinanceDetail {
        metadata: entry.metadata.clone(),
        body,
    };
    let time = GitTimestampKst::from_promulgation_date(&detail.metadata.promulgation_date)
        .unwrap_or_else(|_| GitTimestampKst::from_epoch(0));

    let markdown = ordinance_to_markdown(&detail)?;
    let (blob_sha, compressed_blob) = precompute_blob(&markdown);
    let message = build_commit_message(&detail.metadata);
    Ok(Rendered {
        path: entry.path.clone(),
        markdown,
        blob_sha,
        compressed_blob,
        message,
        time,
    })
}

/// Lists files with the requested extension in deterministic path order.
fn read_sorted_files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for item in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let path = item?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    const SAMPLE_1: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<LawService>
  <자치법규기본정보>
    <자치법규ID>1001</자치법규ID>
    <공포일자>20200101</공포일자>
    <공포번호>1</공포번호>
    <자치법규명><![CDATA[서울특별시 샘플 조례]]></자치법규명>
    <시행일자>20200201</시행일자>
    <자치법규종류>C0001</자치법규종류>
    <지자체기관명>서울특별시</지자체기관명>
    <담당부서명>총무과</담당부서명>
    <제개정정보>제정</제개정정보>
  </자치법규기본정보>
  <조문>
    <조 조문번호='000100'>
      <조문번호>000100</조문번호>
      <조제목><![CDATA[목적]]></조제목>
      <조내용><![CDATA[제1조(목적) 이 조례는 ...]]></조내용>
    </조>
  </조문>
  <부칙>
    <부칙내용><![CDATA[이 조례는 공포한 날부터 시행한다.]]></부칙내용>
  </부칙>
</LawService>"#;

    const SAMPLE_2: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<LawService>
  <자치법규기본정보>
    <자치법규ID>1002</자치법규ID>
    <공포일자>20210101</공포일자>
    <공포번호>2</공포번호>
    <자치법규명><![CDATA[강남구 샘플 규칙]]></자치법규명>
    <시행일자>20210201</시행일자>
    <자치법규종류>C0002</자치법규종류>
    <지자체기관명>서울특별시 강남구</지자체기관명>
    <담당부서명>총무과</담당부서명>
    <제개정정보>제정</제개정정보>
  </자치법규기본정보>
  <조문>
    <조 조문번호='000100'>
      <조문번호>000100</조문번호>
      <조제목><![CDATA[목적]]></조제목>
      <조내용><![CDATA[제1조(목적) 이 규칙은 ...]]></조내용>
    </조>
  </조문>
</LawService>"#;

    const SAMPLE_3: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<LawService>
  <자치법규기본정보>
    <자치법규ID>1003</자치법규ID>
    <공포일자>20220303</공포일자>
    <공포번호>3</공포번호>
    <자치법규명><![CDATA[부산광역시 샘플 조례]]></자치법규명>
    <시행일자>20220403</시행일자>
    <자치법규종류>C0001</자치법규종류>
    <지자체기관명>부산광역시</지자체기관명>
    <담당부서명>총무과</담당부서명>
    <제개정정보>일부개정</제개정정보>
  </자치법규기본정보>
  <조문>
    <조 조문번호='000100'>
      <조문번호>000100</조문번호>
      <조제목><![CDATA[목적]]></조제목>
      <조내용><![CDATA[제1조(목적) 이 조례는 ...]]></조내용>
    </조>
  </조문>
</LawService>"#;

    const SAMPLE_INVALID: &str = r#"<!DOCTYPE html><html><body>err</body></html>"#;

    fn write_sample(cache_dir: &Path, stem: &str, xml: &str) {
        fs::write(cache_dir.join(format!("{stem}.xml")), xml).unwrap();
    }

    fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn end_to_end_builds_bare_repo() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join(".cache").join("ordinance");
        fs::create_dir_all(&cache_dir).unwrap();
        write_sample(&cache_dir, "1001", SAMPLE_1);
        write_sample(&cache_dir, "1002", SAMPLE_2);
        write_sample(&cache_dir, "1003", SAMPLE_3);
        write_sample(&cache_dir, "9999", SAMPLE_INVALID);

        let output = temp.path().join("output.git");
        run(Cli {
            cache_dir: cache_dir.clone(),
            output: output.clone(),
        })
        .unwrap();

        assert_eq!(
            git_stdout(&output, ["symbolic-ref", "--short", "HEAD"]).trim(),
            "main"
        );
        // 1 initial commit + 3 ordinances = 4 commits total.
        assert_eq!(
            git_stdout(&output, ["rev-list", "--count", "HEAD"]).trim(),
            "4"
        );

        let tree = git_stdout(
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
        let names: Vec<&str> = tree.lines().collect();
        assert!(names.contains(&"README.md"));
        assert!(
            names
                .iter()
                .any(|n| n.ends_with("/서울특별시 샘플 조례/본문.md"))
        );
        assert!(
            names
                .iter()
                .any(|n| n.ends_with("/강남구 샘플 규칙/본문.md"))
        );
        assert!(
            names
                .iter()
                .any(|n| n.ends_with("/부산광역시 샘플 조례/본문.md"))
        );

        // Oldest ordinance is applied first: its commit date should be 2020-01-01 KST.
        let dates = git_stdout(&output, ["log", "--pretty=%ai", "--reverse"]);
        let mut lines = dates.lines();
        let initial = lines.next().unwrap();
        assert!(initial.starts_with("1970-01-01") || initial.starts_with("1970-01-02"));
        let second = lines.next().unwrap();
        assert!(
            second.starts_with("2020-01-01"),
            "expected oldest ordinance first: {second}"
        );

        let markdown = git_stdout(
            &output,
            [
                "show",
                "HEAD:ordinances/서울특별시/_본청/조례/서울특별시 샘플 조례/본문.md",
            ],
        );
        assert!(markdown.contains("ordinance_type: 조례"));
        assert!(markdown.contains("자치법규ID: '1001'"));
        assert!(markdown.contains("##### 제1조 (목적)"));

        // Author should be bot on the ordinance commits.
        let author = git_stdout(&output, ["log", "--pretty=%ae", "-1", "HEAD"]);
        assert_eq!(author.trim(), "bot@legalize.kr");
    }
}
