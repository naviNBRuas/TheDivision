//! Document ingestion: extracts text from a PDF/image/plain-text file and
//! stores the extracted text as an ordinary `memory` entry (scope
//! `Knowledge`, source `ExternalContent`) — so it's immediately searchable
//! through `divisi memory search`/`divisi memory search --semantic`
//! rather than a parallel search surface of its own. This module's own
//! `documents` table only tracks the original file plus OCR provenance,
//! referencing that memory entry by id.
//!
//! PDFs are extracted via `pdftotext` (poppler-utils); if that yields no
//! text (a scanned PDF with no text layer), falls back to rasterizing each
//! page with `pdftoppm` and OCR'ing each page image with `tesseract`.
//! Images go straight to `tesseract`. None of these three binaries are a
//! hard dependency of divisi itself — ingestion just errors clearly if
//! the one it needs isn't installed (see `doctor`'s soft check for them).

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use divisi_protocol::{MemoryScope, MemorySource};
use std::path::{Path, PathBuf};

pub struct DocumentInfo {
    pub id: i64,
    pub title: String,
    pub project: Option<String>,
    pub source_path: String,
    pub extracted_chars: i64,
    pub memory_id: i64,
    pub ingested_at: String,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS documents (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            project TEXT,
            source_path TEXT NOT NULL,
            extracted_chars INTEGER NOT NULL,
            memory_id INTEGER NOT NULL,
            ingested_at TEXT NOT NULL
        )",
        (),
    )?;
    Ok(())
}

/// Extracts text from `path` based on its extension, storing the result as
/// a memory entry and a `documents` row (with the original file copied
/// into `documents_dir/<id>/source.<ext>`). `conn` must already have both
/// this module's and `memory`'s schema present (see `notes_db`-style
/// helpers in `handlers.rs`).
pub fn ingest(conn: &Connection, documents_dir: &Path, source: &Path, project: Option<String>, title: Option<String>) -> Result<DocumentInfo> {
    if !source.is_file() {
        bail!("{} is not a file", source.display());
    }
    let text = extract_text(source)?;
    let title = title.unwrap_or_else(|| source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "untitled document".into()));
    let extracted_chars = text.chars().count() as i64;

    let memory_id = crate::memory::store(
        conn,
        crate::memory::NewMemory {
            scope: Some(MemoryScope::Knowledge),
            source: Some(MemorySource::ExternalContent),
            project: project.clone(),
            title: title.clone(),
            content: text,
            ..Default::default()
        },
    )?;

    let ingested_at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO documents (title, project, source_path, extracted_chars, memory_id, ingested_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![title, project, "", extracted_chars, memory_id, ingested_at],
    )
    .context("inserting document row")?;
    let id = conn.last_insert_rowid();

    let dest_dir = documents_dir.join(id.to_string());
    std::fs::create_dir_all(&dest_dir)?;
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    let dest = dest_dir.join(format!("source.{ext}"));
    std::fs::copy(source, &dest).with_context(|| format!("copying {} into {}", source.display(), dest.display()))?;
    let source_path = dest.display().to_string();
    conn.execute("UPDATE documents SET source_path = ?1 WHERE id = ?2", params![source_path, id])?;

    Ok(DocumentInfo { id, title, project, source_path, extracted_chars, memory_id, ingested_at })
}

pub fn list(conn: &Connection, project: Option<&str>) -> Result<Vec<DocumentInfo>> {
    let mut stmt = match project {
        Some(_) => conn.prepare("SELECT * FROM documents WHERE project = ?1 ORDER BY ingested_at DESC")?,
        None => conn.prepare("SELECT * FROM documents ORDER BY ingested_at DESC")?,
    };
    let rows = match project {
        Some(p) => stmt.query_map(params![p], row_to_document)?,
        None => stmt.query_map([], row_to_document)?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collecting documents")
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<DocumentInfo>> {
    conn.query_row("SELECT * FROM documents WHERE id = ?1", params![id], row_to_document).optional().context("querying document by id")
}

fn row_to_document(row: &rusqlite::Row) -> rusqlite::Result<DocumentInfo> {
    Ok(DocumentInfo {
        id: row.get("id")?,
        title: row.get("title")?,
        project: row.get("project")?,
        source_path: row.get("source_path")?,
        extracted_chars: row.get("extracted_chars")?,
        memory_id: row.get("memory_id")?,
        ingested_at: row.get("ingested_at")?,
    })
}

fn extract_text(path: &Path) -> Result<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => extract_pdf(path),
        "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" => ocr_image(path),
        "txt" | "md" | "markdown" => std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display())),
        other => bail!("unsupported document type: .{other} (supported: pdf, png, jpg, jpeg, tiff, bmp, txt, md)"),
    }
}

fn extract_pdf(path: &Path) -> Result<String> {
    let output =
        std::process::Command::new("pdftotext").arg(path).arg("-").output().context("running pdftotext (is poppler-utils installed?)")?;
    if !output.status.success() {
        bail!("pdftotext failed on {}: {}", path.display(), String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if !text.trim().is_empty() {
        return Ok(text);
    }
    // No text layer — likely a scanned PDF. Rasterize each page and OCR it.
    ocr_scanned_pdf(path)
}

fn ocr_scanned_pdf(path: &Path) -> Result<String> {
    let tmp = tempfile::tempdir().context("creating temp dir for pdftoppm output")?;
    let prefix = tmp.path().join("page");
    let status = std::process::Command::new("pdftoppm")
        .arg("-png")
        .arg(path)
        .arg(&prefix)
        .status()
        .context("running pdftoppm (is poppler-utils installed?)")?;
    if !status.success() {
        bail!("pdftoppm failed to rasterize {}", path.display());
    }
    let mut pages: Vec<PathBuf> = std::fs::read_dir(tmp.path())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("png"))
        .collect();
    pages.sort();
    if pages.is_empty() {
        bail!("{} produced no text and no rasterized pages", path.display());
    }
    let mut text = String::new();
    for page in pages {
        text.push_str(&ocr_image(&page)?);
        text.push('\n');
    }
    Ok(text)
}

fn ocr_image(path: &Path) -> Result<String> {
    let output = std::process::Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .output()
        .context("running tesseract (is it installed? e.g. `apt install tesseract-ocr`)")?;
    if !output.status.success() {
        bail!("tesseract failed on {}: {}", path.display(), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid PDF (single page, "Hello World" text) — small enough
    // to embed directly, avoids depending on an external fixture file.
    const MINIMAL_PDF: &str = "%PDF-1.4\n\
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
3 0 obj<</Type/Page/Parent 2 0 R/Resources<</Font<</F1 4 0 R>>>>/MediaBox[0 0 200 100]/Contents 5 0 R>>endobj\n\
4 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\n\
5 0 obj<</Length 44>>\nstream\nBT /F1 24 Tf 10 50 Td (Hello World) Tj ET\nendstream\nendobj\n\
trailer<</Size 6/Root 1 0 R>>\n%%EOF\n";

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        crate::memory::ensure_schema(&conn).unwrap();
        conn
    }

    fn pdftotext_available() -> bool {
        std::process::Command::new("pdftotext").arg("-v").output().is_ok()
    }

    #[test]
    fn extract_text_rejects_unsupported_extension() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("file.xyz");
        std::fs::write(&src, "data").unwrap();
        assert!(extract_text(&src).is_err());
    }

    #[test]
    fn ingest_plain_text_stores_as_searchable_memory_and_tracks_document_row() {
        let conn = test_conn();
        let root = tempfile::tempdir().unwrap();
        let documents_dir = root.path().join("documents");
        let src = root.path().join("notes.txt");
        std::fs::write(&src, "important project decision: use sqlite for storage").unwrap();

        let doc = ingest(&conn, &documents_dir, &src, Some("proj".into()), None).unwrap();
        assert_eq!(doc.title, "notes.txt");
        assert!(doc.extracted_chars > 0);
        assert!(Path::new(&doc.source_path).exists(), "original file should be copied into documents_dir");

        let hits = crate::memory::search(&conn, "sqlite", None, Some("proj")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, doc.memory_id);

        let listed = list(&conn, Some("proj")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(get(&conn, doc.id).unwrap().unwrap().title, "notes.txt");
    }

    #[test]
    fn ingest_real_pdf_extracts_text_via_pdftotext() {
        if !pdftotext_available() {
            return; // best-effort: skip cleanly where poppler-utils isn't installed
        }
        let conn = test_conn();
        let root = tempfile::tempdir().unwrap();
        let documents_dir = root.path().join("documents");
        let src = root.path().join("sample.pdf");
        std::fs::write(&src, MINIMAL_PDF).unwrap();

        let doc = ingest(&conn, &documents_dir, &src, None, Some("greeting".into())).unwrap();
        let hits = crate::memory::search(&conn, "Hello World", None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "greeting");
        assert_eq!(doc.title, "greeting");
    }
}
