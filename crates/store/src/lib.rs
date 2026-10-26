//! der worker besitzt die read-write-verbindung. das history-fenster öffnet eine eigene read-only-verbindung; durch wal laufen beide parallel.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

pub type TakeId = i64;
pub type MeetingId = i64;
/// `(segment_id, speaker_id, locked, embedding)`, von re-clustering-uis genutzt.
pub type SegmentEmbedding = (i64, Option<i64>, bool, Option<Vec<f32>>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::Them => "them",
        }
    }

    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("me") { Self::Me } else { Self::Them }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Me => "Me",
            Self::Them => "Them",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingStatus {
    Recording,
    Processing,
    Done,
    Interrupted,
}

impl MeetingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "processing" => Self::Processing,
            "done" => Self::Done,
            "interrupted" => Self::Interrupted,
            _ => Self::Recording,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiarizationState {
    Running,
    Done,
    Failed,
}

impl DiarizationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    Copied,
    Rerun,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Copied => "copied",
            Self::Rerun => "rerun",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "copied" => Self::Copied,
            "rerun" => Self::Rerun,
            _ => Self::Inserted,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewTake {
    pub created_at: i64,
    pub duration_ms: u64,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub mode_id: Option<String>,
    pub mode_name: Option<String>,
    pub engine: String,
    pub language: Option<String>,
    pub raw_text: String,
    pub final_text: String,
    pub llm: Option<String>,
    pub stt_ms: Option<u64>,
    pub llm_ms: Option<u64>,
    pub audio_file: Option<String>,
    pub outcome: Outcome,
    pub parent_id: Option<TakeId>,
}

#[derive(Clone, Debug)]
pub struct Take {
    pub id: TakeId,
    pub created_at: i64,
    pub duration_ms: u64,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub mode_id: Option<String>,
    pub mode_name: Option<String>,
    pub engine: String,
    pub language: Option<String>,
    pub raw_text: String,
    pub final_text: String,
    pub llm: Option<String>,
    pub stt_ms: Option<u64>,
    pub llm_ms: Option<u64>,
    pub audio_file: Option<String>,
    pub outcome: Outcome,
    pub parent_id: Option<TakeId>,
}

#[derive(Clone, Debug)]
pub struct TakeSummary {
    pub id: TakeId,
    pub created_at: i64,
    pub duration_ms: u64,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub mode_name: Option<String>,
    pub final_text: String,
    pub has_audio: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: Option<String>,
    /// keyset-pagination: nur zeilen, die älter als dieser unix-ms-zeitstempel sind.
    pub before: Option<i64>,
    pub limit: u32,
}

impl Query {
    pub const PAGE: u32 = 200;

    pub fn page(&self) -> u32 {
        if self.limit == 0 { Self::PAGE } else { self.limit }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Retention {
    pub text_days: Option<u32>,
    pub audio_days: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Pruned {
    pub deleted_ids: Vec<TakeId>,
    pub audio_files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub takes: u64,
    pub audio_files: u64,
}

#[derive(Clone, Debug)]
pub struct NewMeeting {
    pub title: String,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub started_at: i64,
}

#[derive(Clone, Debug)]
pub struct NewSegment {
    pub meeting_id: MeetingId,
    pub speaker: Speaker,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub id: i64,
    pub meeting_id: MeetingId,
    pub speaker: Speaker,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// die person innerhalb von "them"; bei "me" immer `None`.
    pub speaker_id: Option<i64>,
    /// gesetzt, wenn der user die bubble zugeordnet hat; re-runs fassen es nicht an.
    pub speaker_locked: bool,
    pub overlap: bool,
}

#[derive(Clone, Debug)]
pub struct MeetingSpeaker {
    pub id: i64,
    pub ordinal: u32,
    pub name: Option<String>,
    pub color: u8,
}

impl MeetingSpeaker {
    pub fn label(&self) -> String {
        match self.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => format!("Speaker {}", self.ordinal),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakerTarget {
    Existing(i64),
    New(usize),
}

#[derive(Clone, Debug)]
pub struct DiarizedSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub speaker: usize,
    pub overlap: bool,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Clone, Debug, Default)]
pub struct DiarizedTranscript {
    pub segments: Vec<DiarizedSegment>,
    pub centroids: Vec<Vec<f32>>,
    /// vorhandene speaker-ids für einen re-run; `None` legt einen speaker an.
    pub matches: Vec<Option<i64>>,
}

#[derive(Clone, Debug)]
pub struct Meeting {
    pub id: MeetingId,
    pub title: String,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub audio_me: Option<String>,
    pub audio_them: Option<String>,
    pub summary: Option<String>,
    pub summary_mode: Option<String>,
    pub status: MeetingStatus,
    pub diarization: Option<DiarizationState>,
    /// wall-clock-millisekunden des ersten "Them"-samples in der datei.
    pub them_offset_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct MeetingSummary {
    pub id: MeetingId,
    pub title: String,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_ms: u64,
    pub status: MeetingStatus,
    pub has_audio: bool,
    pub preview: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("history database does not exist")]
    Missing,
    #[error("unexpected outcome in the database: {0}")]
    Outcome(String),
}

const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE takes (
  id            INTEGER PRIMARY KEY,
  created_at    INTEGER NOT NULL,
  duration_ms   INTEGER NOT NULL,
  app_bundle_id TEXT,
  app_name      TEXT,
  mode_id       TEXT,
  mode_name     TEXT,
  engine        TEXT NOT NULL,
  language      TEXT,
  raw_text      TEXT NOT NULL,
  final_text    TEXT NOT NULL,
  llm           TEXT,
  stt_ms        INTEGER,
  llm_ms        INTEGER,
  audio_file    TEXT,
  outcome       TEXT NOT NULL,
  parent_id     INTEGER REFERENCES takes(id) ON DELETE SET NULL
);
CREATE INDEX takes_created_at ON takes(created_at DESC);

CREATE VIRTUAL TABLE takes_fts USING fts5(
  final_text, raw_text,
  content='takes', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER takes_ai AFTER INSERT ON takes BEGIN
  INSERT INTO takes_fts(rowid, final_text, raw_text) VALUES (new.id, new.final_text, new.raw_text);
END;
CREATE TRIGGER takes_ad AFTER DELETE ON takes BEGIN
  INSERT INTO takes_fts(takes_fts, rowid, final_text, raw_text) VALUES ('delete', old.id, old.final_text, old.raw_text);
END;
"#,
    // fts5-external-content-tabellen brauchen auch einen update-trigger; sonst bleiben beim bearbeiten eines transkripts alte tokens zurück.
    r#"
CREATE TRIGGER takes_au AFTER UPDATE ON takes BEGIN
  INSERT INTO takes_fts(takes_fts, rowid, final_text, raw_text) VALUES ('delete', old.id, old.final_text, old.raw_text);
  INSERT INTO takes_fts(rowid, final_text, raw_text) VALUES (new.id, new.final_text, new.raw_text);
END;
"#,
    r#"
CREATE TABLE meetings (
  id            INTEGER PRIMARY KEY,
  title         TEXT NOT NULL,
  app_bundle_id TEXT,
  app_name      TEXT,
  started_at    INTEGER NOT NULL,
  ended_at      INTEGER,
  audio_me      TEXT,
  audio_them    TEXT,
  summary       TEXT,
  summary_mode  TEXT,
  status        TEXT NOT NULL
);
CREATE INDEX meetings_started ON meetings(started_at DESC);

CREATE TABLE segments (
  id         INTEGER PRIMARY KEY,
  meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  speaker    TEXT NOT NULL,
  start_ms   INTEGER NOT NULL,
  end_ms     INTEGER NOT NULL,
  text       TEXT NOT NULL
);
CREATE INDEX segments_meeting ON segments(meeting_id, start_ms);

CREATE VIRTUAL TABLE segments_fts USING fts5(
  text,
  content='segments', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER segments_ai AFTER INSERT ON segments BEGIN
  INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER segments_ad AFTER DELETE ON segments BEGIN
  INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER segments_au AFTER UPDATE ON segments BEGIN
  INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
  INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
END;
"#,
    r#"
CREATE TABLE meeting_speakers (
  id         INTEGER PRIMARY KEY,
  meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  name       TEXT,
  color      INTEGER NOT NULL,
  centroid   BLOB
);
CREATE INDEX meeting_speakers_meeting ON meeting_speakers(meeting_id);

ALTER TABLE segments ADD COLUMN speaker_id INTEGER REFERENCES meeting_speakers(id) ON DELETE SET NULL;
ALTER TABLE segments ADD COLUMN speaker_locked INTEGER NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN overlap INTEGER NOT NULL DEFAULT 0;
ALTER TABLE segments ADD COLUMN embedding BLOB;

ALTER TABLE meetings ADD COLUMN diarization TEXT;
ALTER TABLE meetings ADD COLUMN them_offset_ms INTEGER;
"#,
];

const COLUMNS: &str = "id, created_at, duration_ms, app_bundle_id, app_name, mode_id, mode_name, \
                       engine, language, raw_text, final_text, llm, stt_ms, llm_ms, audio_file, \
                       outcome, parent_id";

pub struct Store {
    conn: Connection,
}

// TODO: refactor
impl Store {
    /// öffnet (und erstellt) die datenbank und spielt ausstehende migrationen ein.
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        migrate(&conn)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { conn })
    }

    /// öffnet eine bestehende datenbank, ohne sie zu erstellen oder zu migrieren. eine wal-datenbank ohne aktiven schreiber lässt sich nicht immer mit `SQLITE_OPEN_READ_ONLY` öffnen, weil sqlite dann die `-shm`-datei anlegen müsste. dann öffnen wir normal und schalten `query_only` ein.
    pub fn open_read_only(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Err(Error::Missing);
        }
        let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => conn,
            Err(_) => {
                let conn = Connection::open(path)?;
                conn.execute_batch("PRAGMA query_only=ON;")?;
                conn
            }
        };
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        Ok(Self { conn })
    }

    pub fn insert(&self, take: &NewTake) -> Result<TakeId, Error> {
        self.conn.execute(
            "INSERT INTO takes (created_at, duration_ms, app_bundle_id, app_name, mode_id, mode_name, \
             engine, language, raw_text, final_text, llm, stt_ms, llm_ms, audio_file, outcome, parent_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                take.created_at,
                take.duration_ms as i64,
                take.app_bundle_id,
                take.app_name,
                take.mode_id,
                take.mode_name,
                take.engine,
                take.language,
                take.raw_text,
                take.final_text,
                take.llm,
                take.stt_ms.map(|value| value as i64),
                take.llm_ms.map(|value| value as i64),
                take.audio_file,
                take.outcome.as_str(),
                take.parent_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list(&self, query: &Query) -> Result<Vec<TakeSummary>, Error> {
        let limit = i64::from(query.page());
        let query_text = query.text.as_deref().map(fts_query).filter(|text| !text.is_empty());
        let mut rows = Vec::new();
        if let Some(text) = query_text {
            let mut statement = self.conn.prepare(
                "SELECT t.id, t.created_at, t.duration_ms, t.app_bundle_id, t.app_name, t.mode_name, \
                 t.final_text, t.audio_file FROM takes t \
                 JOIN takes_fts f ON f.rowid = t.id \
                 WHERE takes_fts MATCH ?1 AND (?2 IS NULL OR t.created_at < ?2) \
                 ORDER BY t.created_at DESC, t.id DESC LIMIT ?3",
            )?;
            let mapped = statement.query_map(params![text, query.before, limit], summary_from_row)?;
            for row in mapped {
                rows.push(row?);
            }
        } else {
            let mut statement = self.conn.prepare(
                "SELECT id, created_at, duration_ms, app_bundle_id, app_name, mode_name, final_text, \
                 audio_file FROM takes \
                 WHERE (?1 IS NULL OR created_at < ?1) \
                 ORDER BY created_at DESC, id DESC LIMIT ?2",
            )?;
            let mapped = statement.query_map(params![query.before, limit], summary_from_row)?;
            for row in mapped {
                rows.push(row?);
            }
        }
        Ok(rows)
    }

    pub fn get(&self, id: TakeId) -> Result<Option<Take>, Error> {
        let sql = format!("SELECT {COLUMNS} FROM takes WHERE id = ?1");
        self.conn.query_row(&sql, params![id], take_from_row).optional().map_err(Error::from)
    }

    pub fn delete(&self, ids: &[TakeId]) -> Result<Vec<PathBuf>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let transaction = self.conn.unchecked_transaction()?;
        let mut audio = Vec::new();
        {
            let mut select = transaction.prepare("SELECT audio_file FROM takes WHERE id = ?1")?;
            let mut delete = transaction.prepare("DELETE FROM takes WHERE id = ?1")?;
            for id in ids {
                if let Some(path) =
                    select.query_row(params![id], |row| row.get::<_, Option<String>>(0)).optional()?.flatten()
                {
                    audio.push(PathBuf::from(path));
                }
                delete.execute(params![id])?;
            }
        }
        transaction.commit()?;
        Ok(audio)
    }

    /// wendet die retention-policy an. text-ablauf löscht die zeile (und ihr audio); audio-ablauf behält die zeile und wirft nur die datei weg.
    pub fn prune(&self, now_ms: i64, policy: &Retention) -> Result<Pruned, Error> {
        let transaction = self.conn.unchecked_transaction()?;
        let mut pruned = Pruned::default();

        if let Some(days) = policy.text_days {
            let cutoff = now_ms.saturating_sub(days_ms(days));
            {
                let mut select = transaction
                    .prepare("SELECT id, audio_file FROM takes WHERE created_at < ?1 ORDER BY id")?;
                let mut rows = select.query(params![cutoff])?;
                while let Some(row) = rows.next()? {
                    pruned.deleted_ids.push(row.get(0)?);
                    if let Some(path) = row.get::<_, Option<String>>(1)? {
                        pruned.audio_files.push(PathBuf::from(path));
                    }
                }
            }
            transaction.execute("DELETE FROM takes WHERE created_at < ?1", params![cutoff])?;
        }

        if let Some(days) = policy.audio_days {
            let cutoff = now_ms.saturating_sub(days_ms(days));
            {
                let mut select = transaction.prepare(
                    "SELECT audio_file FROM takes WHERE audio_file IS NOT NULL AND created_at < ?1",
                )?;
                let mut rows = select.query(params![cutoff])?;
                while let Some(row) = rows.next()? {
                    if let Ok(path) = row.get::<_, String>(0) {
                        pruned.audio_files.push(PathBuf::from(path));
                    }
                }
            }
            transaction.execute(
                "UPDATE takes SET audio_file = NULL WHERE audio_file IS NOT NULL AND created_at < ?1",
                params![cutoff],
            )?;
        }

        transaction.commit()?;
        pruned.audio_files.sort();
        pruned.audio_files.dedup();
        Ok(pruned)
    }

    pub fn delete_all(&self) -> Result<Vec<PathBuf>, Error> {
        let transaction = self.conn.unchecked_transaction()?;
        let mut audio = Vec::new();
        {
            let mut select =
                transaction.prepare("SELECT audio_file FROM takes WHERE audio_file IS NOT NULL")?;
            let mut rows = select.query([])?;
            while let Some(row) = rows.next()? {
                if let Ok(path) = row.get::<_, String>(0) {
                    audio.push(PathBuf::from(path));
                }
            }
        }
        transaction.execute("DELETE FROM takes", [])?;
        transaction.commit()?;
        Ok(audio)
    }

    pub fn delete_all_audio(&self) -> Result<Vec<PathBuf>, Error> {
        let transaction = self.conn.unchecked_transaction()?;
        let mut audio = Vec::new();
        {
            let mut select =
                transaction.prepare("SELECT audio_file FROM takes WHERE audio_file IS NOT NULL")?;
            let mut rows = select.query([])?;
            while let Some(row) = rows.next()? {
                if let Ok(path) = row.get::<_, String>(0) {
                    audio.push(PathBuf::from(path));
                }
            }
        }
        transaction.execute("UPDATE takes SET audio_file = NULL WHERE audio_file IS NOT NULL", [])?;
        transaction.commit()?;
        Ok(audio)
    }

    pub fn set_audio(&self, id: TakeId, audio_file: &str) -> Result<(), Error> {
        self.conn.execute("UPDATE takes SET audio_file = ?1 WHERE id = ?2", params![audio_file, id])?;
        Ok(())
    }

    pub fn clear_audio(&self, id: TakeId) -> Result<(), Error> {
        self.conn.execute("UPDATE takes SET audio_file = NULL WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn audio_files(&self) -> Result<HashSet<String>, Error> {
        let mut statement = self.conn.prepare("SELECT audio_file FROM takes WHERE audio_file IS NOT NULL")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut files = HashSet::new();
        for row in rows {
            files.insert(row?);
        }
        Ok(files)
    }

    pub fn stats(&self) -> Result<Stats, Error> {
        let takes = self.conn.query_row("SELECT count(*) FROM takes", [], |row| row.get::<_, i64>(0))?;
        let audio_files =
            self.conn.query_row("SELECT count(*) FROM takes WHERE audio_file IS NOT NULL", [], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(Stats { takes: takes as u64, audio_files: audio_files as u64 })
    }

    pub fn insert_meeting(&self, meeting: &NewMeeting) -> Result<MeetingId, Error> {
        self.conn.execute(
            "INSERT INTO meetings (title, app_bundle_id, app_name, started_at, status) \
             VALUES (?1, ?2, ?3, ?4, 'recording')",
            params![meeting.title, meeting.app_bundle_id, meeting.app_name, meeting.started_at,],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn meeting(&self, id: MeetingId) -> Result<Option<Meeting>, Error> {
        let sql = "SELECT id, title, app_bundle_id, app_name, started_at, ended_at, audio_me, \
                   audio_them, summary, summary_mode, status, diarization, them_offset_ms \
                   FROM meetings WHERE id = ?1";
        self.conn.query_row(sql, params![id], meeting_from_row).optional().map_err(Error::from)
    }

    pub fn list_meetings(&self, query: &Query) -> Result<Vec<MeetingSummary>, Error> {
        let limit = i64::from(query.page());
        let search = query.text.as_deref().map(fts_query).filter(|text| !text.is_empty());
        let duration = "COALESCE(m.ended_at - m.started_at, \
                        (SELECT max(s.end_ms) FROM segments s WHERE s.meeting_id = m.id), 0)";
        let preview = "(SELECT s.text FROM segments s WHERE s.meeting_id = m.id \
                       ORDER BY s.start_ms LIMIT 1)";
        let mut rows = Vec::new();
        if let Some(text) = search {
            let sql = format!(
                "SELECT DISTINCT m.id, m.title, m.app_bundle_id, m.app_name, m.started_at, \
                 m.ended_at, {duration}, m.status, \
                 (m.audio_me IS NOT NULL OR m.audio_them IS NOT NULL), {preview} \
                 FROM meetings m JOIN segments s ON s.meeting_id = m.id \
                 JOIN segments_fts f ON f.rowid = s.id \
                 WHERE segments_fts MATCH ?1 AND (?2 IS NULL OR m.started_at < ?2) \
                 ORDER BY m.started_at DESC, m.id DESC LIMIT ?3"
            );
            let mut statement = self.conn.prepare(&sql)?;
            let mapped = statement.query_map(params![text, query.before, limit], meeting_summary_from_row)?;
            for row in mapped {
                rows.push(row?);
            }
        } else {
            let sql = format!(
                "SELECT m.id, m.title, m.app_bundle_id, m.app_name, m.started_at, m.ended_at, \
                 {duration}, m.status, \
                 (m.audio_me IS NOT NULL OR m.audio_them IS NOT NULL), {preview} \
                 FROM meetings m WHERE (?1 IS NULL OR m.started_at < ?1) \
                 ORDER BY m.started_at DESC, m.id DESC LIMIT ?2"
            );
            let mut statement = self.conn.prepare(&sql)?;
            let mapped = statement.query_map(params![query.before, limit], meeting_summary_from_row)?;
            for row in mapped {
                rows.push(row?);
            }
        }
        Ok(rows)
    }

    pub fn segments(&self, meeting_id: MeetingId) -> Result<Vec<Segment>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT id, meeting_id, speaker, start_ms, end_ms, text, speaker_id, speaker_locked, overlap \
             FROM segments WHERE meeting_id = ?1 ORDER BY start_ms, id",
        )?;
        let rows = statement.query_map(params![meeting_id], segment_from_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn insert_segment(&self, segment: &NewSegment) -> Result<i64, Error> {
        self.conn.execute(
            "INSERT INTO segments (meeting_id, speaker, start_ms, end_ms, text) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                segment.meeting_id,
                segment.speaker.as_str(),
                segment.start_ms as i64,
                segment.end_ms as i64,
                segment.text,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn meeting_speakers(&self, meeting_id: MeetingId) -> Result<Vec<MeetingSpeaker>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT id, ordinal, name, color FROM meeting_speakers \
             WHERE meeting_id = ?1 ORDER BY ordinal, id",
        )?;
        let rows = statement.query_map(params![meeting_id], |row| {
            Ok(MeetingSpeaker {
                id: row.get(0)?,
                ordinal: row.get::<_, i64>(1)?.max(1) as u32,
                name: row.get(2)?,
                color: row.get::<_, i64>(3)?.clamp(0, 7) as u8,
            })
        })?;
        let mut speakers = Vec::new();
        for row in rows {
            speakers.push(row?);
        }
        Ok(speakers)
    }

    pub fn speaker_centroids(&self, meeting_id: MeetingId) -> Result<Vec<(i64, Vec<f32>)>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT id, centroid FROM meeting_speakers WHERE meeting_id = ?1 AND centroid IS NOT NULL",
        )?;
        let rows = statement
            .query_map(params![meeting_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, blob) = row?;
            if let Some(vector) = decode_embedding(&blob) {
                out.push((id, vector));
            }
        }
        Ok(out)
    }

    pub fn segment_embeddings(&self, meeting_id: MeetingId) -> Result<Vec<SegmentEmbedding>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT id, speaker_id, speaker_locked, embedding FROM segments \
             WHERE meeting_id = ?1 AND speaker = 'them' ORDER BY start_ms, id",
        )?;
        let rows = statement.query_map(params![meeting_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, speaker, locked, blob) = row?;
            out.push((id, speaker, locked, blob.and_then(|blob| decode_embedding(&blob))));
        }
        Ok(out)
    }

    /// ersetzt alle "them"-segmente in einer transaktion durch die diarisierten turns.
    pub fn replace_them_segments(
        &self,
        meeting_id: MeetingId,
        result: &DiarizedTranscript,
    ) -> Result<(), Error> {
        let transaction = self.conn.unchecked_transaction()?;
        let mut next_ordinal: u32 = transaction.query_row(
            "SELECT COALESCE(max(ordinal), 0) + 1 FROM meeting_speakers WHERE meeting_id = ?1",
            params![meeting_id],
            |row| row.get::<_, i64>(0),
        )? as u32;

        let used =
            result.segments.iter().map(|segment| segment.speaker).collect::<std::collections::BTreeSet<_>>();
        let mut speaker_ids = vec![None; used.iter().copied().max().map(|max| max + 1).unwrap_or(0)];
        for &cluster in &used {
            let matched = result.matches.get(cluster).copied().flatten().filter(|id| {
                transaction
                    .query_row(
                        "SELECT 1 FROM meeting_speakers WHERE id = ?1 AND meeting_id = ?2",
                        params![id, meeting_id],
                        |_| Ok(()),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .is_some()
            });
            let id = match matched {
                Some(id) => id,
                None => {
                    let ordinal = next_ordinal;
                    next_ordinal += 1;
                    transaction.execute(
                        "INSERT INTO meeting_speakers (meeting_id, ordinal, color) VALUES (?1, ?2, ?3)",
                        params![meeting_id, ordinal, color_for(ordinal)],
                    )?;
                    transaction.last_insert_rowid()
                }
            };
            if let Some(centroid) = result.centroids.get(cluster) {
                transaction.execute(
                    "UPDATE meeting_speakers SET centroid = ?1 WHERE id = ?2",
                    params![encode_embedding(centroid), id],
                )?;
            }
            if let Some(slot) = speaker_ids.get_mut(cluster) {
                *slot = Some(id);
            }
        }

        transaction.execute(
            "DELETE FROM segments WHERE meeting_id = ?1 AND speaker = 'them' AND speaker_locked = 0",
            params![meeting_id],
        )?;
        let mut locked_statement = transaction.prepare(
            "SELECT start_ms, end_ms FROM segments \
             WHERE meeting_id = ?1 AND speaker = 'them' AND speaker_locked = 1",
        )?;
        let locked = locked_statement
            .query_map(params![meeting_id], |row| {
                Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, i64>(1)?.max(0) as u64))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(locked_statement);
        {
            let mut insert = transaction.prepare(
                "INSERT INTO segments (meeting_id, speaker, start_ms, end_ms, text, speaker_id, overlap, embedding) \
                 VALUES (?1, 'them', ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for segment in &result.segments {
                // eine gelockte bubble ist die entscheidung des users; ein re-run darf daneben keine zweite kopie anlegen.
                let duration = segment.end_ms.saturating_sub(segment.start_ms).max(1);
                let duplicated = locked.iter().any(|(start, end)| {
                    let overlap = segment.end_ms.min(*end).saturating_sub(segment.start_ms.max(*start));
                    overlap * 2 > duration
                });
                if duplicated {
                    continue;
                }
                let speaker_id = speaker_ids.get(segment.speaker).copied().flatten();
                let embedding = segment.embedding.as_deref().map(encode_embedding);
                insert.execute(params![
                    meeting_id,
                    segment.start_ms as i64,
                    segment.end_ms as i64,
                    segment.text,
                    speaker_id,
                    i64::from(segment.overlap),
                    embedding,
                ])?;
            }
        }
        dedupe_echo(&transaction, meeting_id)?;
        transaction.execute(
            "DELETE FROM meeting_speakers WHERE meeting_id = ?1 AND id NOT IN \
             (SELECT speaker_id FROM segments WHERE meeting_id = ?1 AND speaker_id IS NOT NULL)",
            params![meeting_id],
        )?;
        transaction.execute("UPDATE meetings SET diarization = 'done' WHERE id = ?1", params![meeting_id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn rename_speaker(&self, speaker_id: i64, name: Option<&str>) -> Result<(), Error> {
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        self.conn
            .execute("UPDATE meeting_speakers SET name = ?1 WHERE id = ?2", params![name, speaker_id])?;
        Ok(())
    }

    /// verschiebt segmente auf einen speaker und sperrt sie für re-runs.
    pub fn assign_segments(&self, segment_ids: &[i64], speaker_id: i64) -> Result<(), Error> {
        let transaction = self.conn.unchecked_transaction()?;
        for segment_id in segment_ids {
            transaction.execute(
                "UPDATE segments SET speaker_id = ?1, speaker_locked = 1 WHERE id = ?2",
                params![speaker_id, segment_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn new_speaker_for(&self, segment_ids: &[i64]) -> Result<i64, Error> {
        let Some(first) = segment_ids.first() else {
            return Err(Error::Outcome("no segments to assign".into()));
        };
        let meeting_id: MeetingId =
            self.conn.query_row("SELECT meeting_id FROM segments WHERE id = ?1", params![first], |row| {
                row.get(0)
            })?;
        let id = self.insert_speaker(meeting_id)?;
        self.assign_segments(segment_ids, id)?;
        self.recompute_centroid(id)?;
        Ok(id)
    }

    fn insert_speaker(&self, meeting_id: MeetingId) -> Result<i64, Error> {
        let ordinal: u32 = self.conn.query_row(
            "SELECT COALESCE(max(ordinal), 0) + 1 FROM meeting_speakers WHERE meeting_id = ?1",
            params![meeting_id],
            |row| row.get::<_, i64>(0),
        )? as u32;
        self.conn.execute(
            "INSERT INTO meeting_speakers (meeting_id, ordinal, color) VALUES (?1, ?2, ?3)",
            params![meeting_id, ordinal, color_for(ordinal)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn speaker_meeting(&self, speaker_id: i64) -> Result<MeetingId, Error> {
        Ok(self.conn.query_row(
            "SELECT meeting_id FROM meeting_speakers WHERE id = ?1",
            params![speaker_id],
            |row| row.get(0),
        )?)
    }

    /// wendet ein re-clustering an: verschiebt turns (ohne sie zu locken), legt nötige speaker an und löscht speaker ohne turns.
    pub fn reassign_segments(
        &self,
        meeting_id: MeetingId,
        moves: &[(i64, SpeakerTarget)],
    ) -> Result<(), Error> {
        let transaction = self.conn.unchecked_transaction()?;
        let mut created = std::collections::BTreeMap::new();
        for (segment_id, target) in moves {
            let speaker_id = match *target {
                SpeakerTarget::Existing(id) => id,
                SpeakerTarget::New(key) => match created.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = self.insert_speaker(meeting_id)?;
                        created.insert(key, id);
                        id
                    }
                },
            };
            transaction.execute(
                "UPDATE segments SET speaker_id = ?1 WHERE id = ?2 AND meeting_id = ?3",
                params![speaker_id, segment_id, meeting_id],
            )?;
        }
        transaction.execute(
            "DELETE FROM meeting_speakers WHERE meeting_id = ?1 AND id NOT IN \
             (SELECT speaker_id FROM segments WHERE meeting_id = ?1 AND speaker_id IS NOT NULL)",
            params![meeting_id],
        )?;
        transaction.commit()?;
        for speaker in self.meeting_speakers(meeting_id)? {
            self.recompute_centroid(speaker.id)?;
        }
        Ok(())
    }

    /// verschiebt alle segmente von einem speaker zum anderen und löscht die quelle. speaker aus verschiedenen meetings werden nie gemerged.
    pub fn merge_speakers(&self, from: i64, into: i64) -> Result<(), Error> {
        if from == into || self.speaker_meeting(from)? != self.speaker_meeting(into)? {
            return Ok(());
        }
        let transaction = self.conn.unchecked_transaction()?;
        transaction
            .execute("UPDATE segments SET speaker_id = ?1 WHERE speaker_id = ?2", params![into, from])?;
        transaction.execute(
            "UPDATE meeting_speakers SET name = COALESCE(name, \
             (SELECT name FROM meeting_speakers WHERE id = ?2)) WHERE id = ?1",
            params![into, from],
        )?;
        transaction.execute("DELETE FROM meeting_speakers WHERE id = ?1", params![from])?;
        transaction.commit()?;
        self.recompute_centroid(into)
    }

    /// teilt einen speaker anhand von gruppen von segment-ids. die erste gruppe behält den speaker; spätere gruppen bekommen neue speaker und werden gelockt.
    pub fn split_speaker(&self, speaker_id: i64, groups: &[Vec<i64>]) -> Result<Vec<i64>, Error> {
        if groups.is_empty() {
            return Ok(Vec::new());
        }
        let meeting_id = self.speaker_meeting(speaker_id)?;
        let transaction = self.conn.unchecked_transaction()?;
        let mut ids = vec![speaker_id];
        for group in groups.iter().skip(1) {
            let id = self.insert_speaker(meeting_id)?;
            for segment in group {
                transaction.execute(
                    "UPDATE segments SET speaker_id = ?1, speaker_locked = 1 \
                     WHERE id = ?2 AND speaker_locked = 0",
                    params![id, segment],
                )?;
            }
            ids.push(id);
        }
        transaction.commit()?;
        for id in &ids {
            self.recompute_centroid(*id)?;
        }
        Ok(ids)
    }

    fn recompute_centroid(&self, speaker_id: i64) -> Result<(), Error> {
        let mut statement = self
            .conn
            .prepare("SELECT embedding FROM segments WHERE speaker_id = ?1 AND embedding IS NOT NULL")?;
        let rows = statement.query_map(params![speaker_id], |row| row.get::<_, Vec<u8>>(0))?;
        let mut vectors = Vec::new();
        for row in rows {
            if let Some(vector) = decode_embedding(&row?) {
                vectors.push(vector);
            }
        }
        // turns ohne voice-daten sagen nichts aus; dann behalten wir den alten centroid.
        let Some(centroid) = mean_embedding(&vectors).map(|vector| encode_embedding(&vector)) else {
            return Ok(());
        };
        self.conn.execute(
            "UPDATE meeting_speakers SET centroid = ?1 WHERE id = ?2",
            params![centroid, speaker_id],
        )?;
        Ok(())
    }

    pub fn set_diarization(
        &self,
        meeting_id: MeetingId,
        state: Option<DiarizationState>,
    ) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE meetings SET diarization = ?1 WHERE id = ?2",
            params![state.map(DiarizationState::as_str), meeting_id],
        )?;
        Ok(())
    }

    pub fn set_them_offset(&self, meeting_id: MeetingId, offset_ms: u64) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE meetings SET them_offset_ms = ?1 WHERE id = ?2",
            params![offset_ms as i64, meeting_id],
        )?;
        Ok(())
    }

    pub fn set_meeting_status(&self, id: MeetingId, status: MeetingStatus) -> Result<(), Error> {
        self.conn.execute("UPDATE meetings SET status = ?1 WHERE id = ?2", params![status.as_str(), id])?;
        Ok(())
    }

    pub fn finish_meeting(&self, id: MeetingId, ended_at: i64) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE meetings SET ended_at = ?1, status = 'processing' WHERE id = ?2",
            params![ended_at, id],
        )?;
        Ok(())
    }

    pub fn set_meeting_audio(
        &self,
        id: MeetingId,
        me: Option<&str>,
        them: Option<&str>,
    ) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE meetings SET audio_me = ?1, audio_them = ?2 WHERE id = ?3",
            params![me, them, id],
        )?;
        Ok(())
    }

    pub fn set_meeting_summary(&self, id: MeetingId, summary: &str, mode: &str) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE meetings SET summary = ?1, summary_mode = ?2, status = 'done' WHERE id = ?3",
            params![summary, mode, id],
        )?;
        Ok(())
    }

    pub fn set_meeting_title(&self, id: MeetingId, title: &str) -> Result<(), Error> {
        self.conn.execute("UPDATE meetings SET title = ?1 WHERE id = ?2", params![title, id])?;
        Ok(())
    }

    /// markiert beim start noch laufende aufnahmen als interrupted und laufende diarization als failed.
    pub fn mark_interrupted(&self) -> Result<usize, Error> {
        self.conn.execute("UPDATE meetings SET diarization = 'failed' WHERE diarization = 'running'", [])?;
        Ok(self.conn.execute("UPDATE meetings SET status = 'interrupted' WHERE status = 'recording'", [])?)
    }

    pub fn delete_meetings(&self, ids: &[MeetingId]) -> Result<Vec<PathBuf>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let transaction = self.conn.unchecked_transaction()?;
        let mut audio = Vec::new();
        {
            let mut select =
                transaction.prepare("SELECT audio_me, audio_them FROM meetings WHERE id = ?1")?;
            let mut delete = transaction.prepare("DELETE FROM meetings WHERE id = ?1")?;
            for id in ids {
                let row = select
                    .query_row(params![id], |row| {
                        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?))
                    })
                    .optional()?;
                if let Some((me, them)) = row {
                    audio.extend(me.map(PathBuf::from));
                    audio.extend(them.map(PathBuf::from));
                }
                delete.execute(params![id])?;
            }
        }
        transaction.commit()?;
        Ok(audio)
    }

    pub fn meeting_audio_files(&self) -> Result<HashSet<String>, Error> {
        let mut statement = self.conn.prepare(
            "SELECT audio_me, audio_them FROM meetings WHERE audio_me IS NOT NULL OR audio_them IS NOT NULL",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)))?;
        let mut files = HashSet::new();
        for row in rows {
            let (me, them) = row?;
            files.extend(me);
            files.extend(them);
        }
        Ok(files)
    }

    pub fn delete_all_meetings(&self) -> Result<Vec<PathBuf>, Error> {
        let mut audio = Vec::new();
        {
            let mut statement =
                self.conn.prepare("SELECT audio_me, audio_them FROM meetings WHERE audio_me IS NOT NULL OR audio_them IS NOT NULL")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                if let Ok(me) = row.get::<_, Option<String>>(0) {
                    audio.extend(me.map(PathBuf::from));
                }
                if let Ok(them) = row.get::<_, Option<String>>(1) {
                    audio.extend(them.map(PathBuf::from));
                }
            }
        }
        self.conn.execute("DELETE FROM meetings", [])?;
        Ok(audio)
    }

    pub fn delete_all_meeting_audio(&self) -> Result<Vec<PathBuf>, Error> {
        let mut audio = Vec::new();
        {
            let mut statement =
                self.conn.prepare("SELECT audio_me, audio_them FROM meetings WHERE audio_me IS NOT NULL OR audio_them IS NOT NULL")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                if let Ok(me) = row.get::<_, Option<String>>(0) {
                    audio.extend(me.map(PathBuf::from));
                }
                if let Ok(them) = row.get::<_, Option<String>>(1) {
                    audio.extend(them.map(PathBuf::from));
                }
            }
        }
        self.conn.execute("UPDATE meetings SET audio_me = NULL, audio_them = NULL", [])?;
        self.conn.execute("UPDATE segments SET embedding = NULL", [])?;
        self.conn.execute("UPDATE meeting_speakers SET centroid = NULL", [])?;
        Ok(audio)
    }

    pub fn prune_meeting_audio(&self, now_ms: i64, audio_days: Option<u32>) -> Result<Vec<PathBuf>, Error> {
        let Some(days) = audio_days else {
            return Ok(Vec::new());
        };
        let cutoff = now_ms.saturating_sub(days_ms(days));
        let transaction = self.conn.unchecked_transaction()?;
        let mut audio = Vec::new();
        {
            let mut select = transaction.prepare(
                "SELECT audio_me, audio_them FROM meetings \
                 WHERE started_at < ?1 AND (audio_me IS NOT NULL OR audio_them IS NOT NULL)",
            )?;
            let mut rows = select.query(params![cutoff])?;
            while let Some(row) = rows.next()? {
                if let Ok(me) = row.get::<_, Option<String>>(0) {
                    audio.extend(me.map(PathBuf::from));
                }
                if let Ok(them) = row.get::<_, Option<String>>(1) {
                    audio.extend(them.map(PathBuf::from));
                }
            }
        }
        transaction.execute(
            "UPDATE meetings SET audio_me = NULL, audio_them = NULL \
             WHERE started_at < ?1 AND (audio_me IS NOT NULL OR audio_them IS NOT NULL)",
            params![cutoff],
        )?;
        // voice-embeddings gehören zu den audiodaten und verschwinden mit ihnen.
        transaction.execute(
            "UPDATE segments SET embedding = NULL WHERE meeting_id IN \
             (SELECT id FROM meetings WHERE started_at < ?1)",
            params![cutoff],
        )?;
        transaction.execute(
            "UPDATE meeting_speakers SET centroid = NULL WHERE meeting_id IN \
             (SELECT id FROM meetings WHERE started_at < ?1)",
            params![cutoff],
        )?;
        transaction.commit()?;
        Ok(audio)
    }

    pub fn fts_enabled(&self) -> Result<bool, Error> {
        let mut statement = self.conn.prepare("PRAGMA compile_options")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            if row?.contains("ENABLE_FTS5") {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn days_ms(days: u32) -> i64 {
    i64::from(days) * 24 * 60 * 60 * 1000
}

fn color_for(ordinal: u32) -> i64 {
    i64::from(ordinal.saturating_sub(1) % 8)
}

fn encode_embedding(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_embedding(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    Some(bytes.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)).collect())
}

fn mean_embedding(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    let mut out = vec![0.0f32; first.len()];
    for vector in vectors {
        for (slot, value) in out.iter_mut().zip(vector) {
            *slot += value;
        }
    }
    let count = vectors.len() as f32;
    for value in &mut out {
        *value /= count;
    }
    let norm = out.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in &mut out {
            *value /= norm;
        }
    }
    Some(out)
}

/// löscht "me"-segmente, die einen frischen "them"-turn doppeln: zeitlich überlappend und fast gleicher text. echo, das übers mikrofon leckt.
fn dedupe_echo(conn: &Connection, meeting_id: MeetingId) -> Result<(), Error> {
    let mut them_statement = conn
        .prepare("SELECT start_ms, end_ms, text FROM segments WHERE meeting_id = ?1 AND speaker = 'them'")?;
    let them = them_statement
        .query_map(params![meeting_id], |row| {
            Ok((
                row.get::<_, i64>(0)?.max(0) as u64,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut me_statement = conn.prepare(
        "SELECT id, start_ms, end_ms, text FROM segments WHERE meeting_id = ?1 AND speaker = 'me'",
    )?;
    let me = me_statement
        .query_map(params![meeting_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, i64>(2)?.max(0) as u64,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut delete = conn.prepare("DELETE FROM segments WHERE id = ?1")?;
    for (id, start, end, text) in me {
        let duration = end.saturating_sub(start).max(1);
        let duplicate = them.iter().any(|(them_start, them_end, them_text)| {
            let overlap = end.min(*them_end).saturating_sub(start.max(*them_start));
            overlap * 2 > duration && text_similarity(&text, them_text) >= 0.8
        });
        if duplicate {
            delete.execute(params![id])?;
        }
    }
    Ok(())
}

fn text_similarity(left: &str, right: &str) -> f32 {
    let tokens = |text: &str| {
        text.to_lowercase()
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let left = tokens(left);
    let right = tokens(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let mut matched = 0usize;
    let mut used = vec![false; right.len()];
    for token in &left {
        if let Some(index) =
            right.iter().enumerate().position(|(index, other)| !used[index] && other == token)
        {
            used[index] = true;
            matched += 1;
        }
    }
    (2 * matched) as f32 / (left.len() + right.len()) as f32
}

fn migrate(conn: &Connection) -> Result<(), Error> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        let transaction = conn.unchecked_transaction()?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", (index + 1) as i64)?;
        transaction.commit()?;
    }
    Ok(())
}

/// quotet jedes whitespace-getrennte token und macht daraus eine fts-prefix-query. rohe fts-syntax vom user erreicht sqlite nie.
fn fts_query(text: &str) -> String {
    text.split_whitespace()
        .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TakeSummary> {
    Ok(TakeSummary {
        id: row.get(0)?,
        created_at: row.get(1)?,
        duration_ms: row.get::<_, i64>(2)? as u64,
        app_bundle_id: row.get(3)?,
        app_name: row.get(4)?,
        mode_name: row.get(5)?,
        final_text: row.get(6)?,
        has_audio: row.get::<_, Option<String>>(7)?.is_some(),
    })
}

fn take_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Take> {
    Ok(Take {
        id: row.get(0)?,
        created_at: row.get(1)?,
        duration_ms: row.get::<_, i64>(2)? as u64,
        app_bundle_id: row.get(3)?,
        app_name: row.get(4)?,
        mode_id: row.get(5)?,
        mode_name: row.get(6)?,
        engine: row.get(7)?,
        language: row.get(8)?,
        raw_text: row.get(9)?,
        final_text: row.get(10)?,
        llm: row.get(11)?,
        stt_ms: row.get::<_, Option<i64>>(12)?.map(|value| value as u64),
        llm_ms: row.get::<_, Option<i64>>(13)?.map(|value| value as u64),
        audio_file: row.get(14)?,
        outcome: Outcome::parse(&row.get::<_, String>(15)?),
        parent_id: row.get(16)?,
    })
}

fn meeting_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Meeting> {
    Ok(Meeting {
        id: row.get(0)?,
        title: row.get(1)?,
        app_bundle_id: row.get(2)?,
        app_name: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        audio_me: row.get(6)?,
        audio_them: row.get(7)?,
        summary: row.get(8)?,
        summary_mode: row.get(9)?,
        status: MeetingStatus::parse(&row.get::<_, String>(10)?),
        diarization: row.get::<_, Option<String>>(11)?.as_deref().and_then(DiarizationState::parse),
        them_offset_ms: row.get::<_, Option<i64>>(12)?.map(|value| value.max(0) as u64),
    })
}

fn meeting_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MeetingSummary> {
    Ok(MeetingSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        app_bundle_id: row.get(2)?,
        app_name: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        duration_ms: row.get::<_, i64>(6)?.max(0) as u64,
        status: MeetingStatus::parse(&row.get::<_, String>(7)?),
        has_audio: row.get::<_, i64>(8)? != 0,
        preview: row.get(9)?,
    })
}

fn segment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        speaker: Speaker::parse(&row.get::<_, String>(2)?),
        start_ms: row.get::<_, i64>(3)?.max(0) as u64,
        end_ms: row.get::<_, i64>(4)?.max(0) as u64,
        text: row.get(5)?,
        speaker_id: row.get(6)?,
        speaker_locked: row.get::<_, i64>(7)? != 0,
        overlap: row.get::<_, i64>(8)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tf-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn take(text: &str, created_at: i64) -> NewTake {
        NewTake {
            created_at,
            duration_ms: 4200,
            app_bundle_id: Some("com.apple.Safari".into()),
            app_name: Some("Safari".into()),
            mode_id: Some("raw".into()),
            mode_name: Some("Raw".into()),
            engine: "parakeet-tdt-0.6b-v3".into(),
            language: Some("en".into()),
            raw_text: text.into(),
            final_text: text.into(),
            llm: None,
            stt_ms: Some(98),
            llm_ms: None,
            audio_file: None,
            outcome: Outcome::Inserted,
            parent_id: None,
        }
    }

    #[test]
    fn fts5_is_available() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        assert!(store.fts_enabled().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn inserts_and_searches_with_prefix_and_diacritics() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        store.insert(&take("Let's move the sync to Thursday at 3.", 1_000)).unwrap();
        store.insert(&take("We met at the café", 2_000)).unwrap();
        store.insert(&take("Unrelated note", 3_000)).unwrap();

        let find = |text: &str| {
            store
                .list(&Query { text: Some(text.into()), before: None, limit: 200 })
                .unwrap()
                .into_iter()
                .map(|take| take.final_text)
                .collect::<Vec<_>>()
        };
        assert_eq!(find("thurs"), vec!["Let's move the sync to Thursday at 3."]);
        assert_eq!(find("cafe"), vec!["We met at the café"]);
        assert_eq!(find("thurs café"), Vec::<String>::new());
        assert_eq!(find("\"quoted").len(), 0);

        let all = store.list(&Query::default()).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].created_at, 3_000);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pages_newest_first() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        for index in 0..10 {
            store.insert(&take(&format!("take {index}"), index)).unwrap();
        }
        let first = store.list(&Query { limit: 4, ..Query::default() }).unwrap();
        assert_eq!(first.len(), 4);
        let before = first.last().unwrap().created_at;
        let second = store.list(&Query { before: Some(before), limit: 4, ..Query::default() }).unwrap();
        assert_eq!(second.len(), 4);
        assert!(second.iter().all(|take| take.created_at < before));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn delete_removes_rows_and_fts_entries() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert(&take("Delete the dragonfruit note", 1_000)).unwrap();
        store.insert(&take("Keep this one", 2_000)).unwrap();
        store.set_audio(id, "2026/10/1.m4a").unwrap();

        let audio = store.delete(&[id]).unwrap();
        assert_eq!(audio, vec![PathBuf::from("2026/10/1.m4a")]);
        assert!(store.get(id).unwrap().is_none());
        let results = store.list(&Query { text: Some("dragonfruit".into()), ..Query::default() }).unwrap();
        assert!(results.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn audio_expiry_keeps_the_row() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert(&take("Old but useful", 1)).unwrap();
        store.set_audio(id, "2026/01/1.m4a").unwrap();
        let pruned = store
            .prune(60 * 24 * 60 * 60 * 1000, &Retention { text_days: None, audio_days: Some(1) })
            .unwrap();
        assert_eq!(pruned.audio_files, vec![PathBuf::from("2026/01/1.m4a")]);
        let row = store.get(id).unwrap().unwrap();
        assert!(row.audio_file.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn text_expiry_deletes_rows_and_returns_their_audio() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let old = store.insert(&take("Old note", 1)).unwrap();
        store.set_audio(old, "2026/01/1.m4a").unwrap();
        let pruned = store
            .prune(60 * 24 * 60 * 60 * 1000, &Retention { text_days: Some(1), audio_days: None })
            .unwrap();
        assert_eq!(pruned.deleted_ids, vec![old]);
        assert_eq!(pruned.audio_files, vec![PathBuf::from("2026/01/1.m4a")]);
        assert!(store.get(old).unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn delete_all_clears_everything() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        for index in 0..5 {
            store.insert(&take(&format!("note {index}"), index)).unwrap();
        }
        assert_eq!(store.delete_all().unwrap().len(), 0);
        assert_eq!(store.stats().unwrap().takes, 0);
        assert!(store.list(&Query { text: Some("note".into()), ..Query::default() }).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn delete_all_audio_keeps_transcripts() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert(&take("keep my words", 1)).unwrap();
        store.set_audio(id, "2026/01/1.m4a").unwrap();
        let files = store.delete_all_audio().unwrap();
        assert_eq!(files, vec![PathBuf::from("2026/01/1.m4a")]);
        let row = store.get(id).unwrap().unwrap();
        assert!(row.audio_file.is_none());
        assert_eq!(row.final_text, "keep my words");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reopens_without_replaying_migrations() {
        let dir = temp_dir();
        let path = dir.join("history.sqlite");
        {
            let store = Store::open(&path).unwrap();
            store.insert(&take("persist me", 1)).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.stats().unwrap().takes, 1);
        let read_only = Store::open_read_only(&path).unwrap();
        assert_eq!(read_only.stats().unwrap().takes, 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rerun_rows_link_to_the_parent() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let parent = store.insert(&take("first pass", 1)).unwrap();
        let mut rerun = take("second pass", 2);
        rerun.outcome = Outcome::Rerun;
        rerun.parent_id = Some(parent);
        let child = store.insert(&rerun).unwrap();
        let row = store.get(child).unwrap().unwrap();
        assert_eq!(row.parent_id, Some(parent));
        assert_eq!(row.outcome, Outcome::Rerun);
        fs::remove_dir_all(dir).unwrap();
    }

    fn new_meeting(title: &str, started_at: i64) -> NewMeeting {
        NewMeeting {
            title: title.to_owned(),
            app_bundle_id: Some("us.zoom.xos".into()),
            app_name: Some("Zoom".into()),
            started_at,
        }
    }

    #[test]
    fn meeting_segments_round_trip_and_search() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Roadmap call", 1_000)).unwrap();
        store
            .insert_segment(&NewSegment {
                meeting_id: id,
                speaker: Speaker::Them,
                start_ms: 2_000,
                end_ms: 4_000,
                text: "Let's start with the roadmap".into(),
            })
            .unwrap();
        store
            .insert_segment(&NewSegment {
                meeting_id: id,
                speaker: Speaker::Me,
                start_ms: 4_100,
                end_ms: 5_000,
                text: "Yes, loud and clear".into(),
            })
            .unwrap();
        store.finish_meeting(id, 6_000).unwrap();

        let segments = store.segments(id).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].speaker, Speaker::Them);
        let meeting = store.meeting(id).unwrap().unwrap();
        assert_eq!(meeting.status, MeetingStatus::Processing);
        assert_eq!(meeting.ended_at, Some(6_000));

        let rows = store.list_meetings(&Query { text: Some("roadmap".into()), ..Query::default() }).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].duration_ms, 5_000);
        assert!(rows[0].preview.as_deref().unwrap().contains("roadmap"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn interrupted_meetings_survive_and_keep_their_duration() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Crashy call", 1_000)).unwrap();
        store
            .insert_segment(&NewSegment {
                meeting_id: id,
                speaker: Speaker::Me,
                start_ms: 0,
                end_ms: 180_000,
                text: "A long stretch".into(),
            })
            .unwrap();
        assert_eq!(store.mark_interrupted().unwrap(), 1);
        let meeting = store.meeting(id).unwrap().unwrap();
        assert_eq!(meeting.status, MeetingStatus::Interrupted);
        let rows = store.list_meetings(&Query::default()).unwrap();
        assert_eq!(rows[0].duration_ms, 180_000);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn meeting_audio_and_summary_are_attached() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Call", 1_000)).unwrap();
        store.set_meeting_audio(id, Some("meetings/1/me.m4a"), Some("meetings/1/them.m4a")).unwrap();
        store.set_meeting_summary(id, "## Summary\nAll good", "meeting-summary").unwrap();
        let meeting = store.meeting(id).unwrap().unwrap();
        assert_eq!(meeting.status, MeetingStatus::Done);
        assert_eq!(meeting.summary_mode.as_deref(), Some("meeting-summary"));
        assert_eq!(store.meeting_audio_files().unwrap().len(), 2);
        let deleted = store.delete_meetings(&[id]).unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(store.meeting(id).unwrap().is_none());
        assert!(store.segments(id).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn meeting_audio_expiry_keeps_the_transcript() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Old call", 1)).unwrap();
        store.set_meeting_audio(id, Some("meetings/1/me.m4a"), None).unwrap();
        let files = store.prune_meeting_audio(90 * 24 * 60 * 60 * 1000, Some(30)).unwrap();
        assert_eq!(files, vec![PathBuf::from("meetings/1/me.m4a")]);
        let meeting = store.meeting(id).unwrap().unwrap();
        assert!(meeting.audio_me.is_none());
        assert_eq!(meeting.title, "Old call");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migration_4_upgrades_an_m6_database() {
        let dir = temp_dir();
        let path = dir.join("history.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            for (index, migration) in MIGRATIONS.iter().take(3).enumerate() {
                conn.execute_batch(migration).unwrap();
                conn.pragma_update(None, "user_version", (index + 1) as i64).unwrap();
            }
            conn.execute(
                "INSERT INTO meetings (title, started_at, status) VALUES ('team call', 1000, 'processing')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO segments (meeting_id, speaker, start_ms, end_ms, text) \
                 VALUES (1, 'them', 0, 1000, 'hello')",
                [],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let meeting = store.meeting(1).unwrap().unwrap();
        assert_eq!(meeting.diarization, None);
        assert_eq!(meeting.them_offset_ms, None);
        let segments = store.segments(1).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].speaker_id, None);
        assert!(!segments[0].speaker_locked);
        store
            .replace_them_segments(
                1,
                &DiarizedTranscript {
                    segments: vec![DiarizedSegment {
                        start_ms: 0,
                        end_ms: 1_000,
                        text: "hello".into(),
                        speaker: 0,
                        overlap: false,
                        embedding: None,
                    }],
                    centroids: vec![vec![1.0, 0.0]],
                    matches: vec![None],
                },
            )
            .unwrap();
        assert_eq!(store.meeting_speakers(1).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn diarized_turns_replace_them_and_keep_names() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Team call", 1_000)).unwrap();
        store
            .insert_segment(&NewSegment {
                meeting_id: id,
                speaker: Speaker::Them,
                start_ms: 0,
                end_ms: 1_000,
                text: "old mixed segment".into(),
            })
            .unwrap();
        let result = DiarizedTranscript {
            segments: vec![
                DiarizedSegment {
                    start_ms: 0,
                    end_ms: 2_000,
                    text: "Hi everyone".into(),
                    speaker: 0,
                    overlap: false,
                    embedding: Some(vec![1.0, 0.0]),
                },
                DiarizedSegment {
                    start_ms: 2_100,
                    end_ms: 4_000,
                    text: "Yes, loud and clear".into(),
                    speaker: 1,
                    overlap: true,
                    embedding: Some(vec![0.0, 1.0]),
                },
            ],
            centroids: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            matches: vec![None, None],
        };
        store.replace_them_segments(id, &result).unwrap();
        assert_eq!(store.meeting(id).unwrap().unwrap().diarization, Some(DiarizationState::Done));
        let speakers = store.meeting_speakers(id).unwrap();
        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0].label(), "Speaker 1");
        let segments = store.segments(id).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].speaker_id, Some(speakers[0].id));
        assert!(segments[1].overlap);

        store.rename_speaker(speakers[0].id, Some("Anna")).unwrap();
        assert_eq!(store.meeting_speakers(id).unwrap()[0].label(), "Anna");

        store.assign_segments(&[segments[1].id], speakers[0].id).unwrap();
        let segments = store.segments(id).unwrap();
        assert!(segments[1].speaker_locked);
        assert_eq!(segments[1].speaker_id, Some(speakers[0].id));
        fs::remove_dir_all(dir).unwrap();
    }

    fn three_turn_meeting(store: &Store) -> MeetingId {
        let id = store.insert_meeting(&new_meeting("Team call", 1_000)).unwrap();
        let turn = |start: u64, speaker: usize, embedding: Vec<f32>| DiarizedSegment {
            start_ms: start,
            end_ms: start + 1_000,
            text: format!("turn {start}"),
            speaker,
            overlap: false,
            embedding: Some(embedding),
        };
        let result = DiarizedTranscript {
            segments: vec![
                turn(0, 0, vec![1.0, 0.0]),
                turn(1_000, 0, vec![1.0, 0.0]),
                turn(2_000, 1, vec![0.0, 1.0]),
            ],
            centroids: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            matches: vec![None, None],
        };
        store.replace_them_segments(id, &result).unwrap();
        id
    }

    #[test]
    fn a_grouped_bubble_moves_all_its_turns_at_once() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = three_turn_meeting(&store);
        let speakers = store.meeting_speakers(id).unwrap();
        let ids = store.segments(id).unwrap().iter().map(|segment| segment.id).collect::<Vec<_>>();

        store.assign_segments(&ids[..2], speakers[1].id).unwrap();
        let segments = store.segments(id).unwrap();
        assert!(segments.iter().all(|segment| segment.speaker_id == Some(speakers[1].id)));
        assert!(segments[..2].iter().all(|segment| segment.speaker_locked));

        let created = store.new_speaker_for(&ids[..2]).unwrap();
        let segments = store.segments(id).unwrap();
        assert_eq!(segments[0].speaker_id, Some(created));
        assert_eq!(segments[1].speaker_id, Some(created));
        assert_eq!(segments[2].speaker_id, Some(speakers[1].id));
    }

    #[test]
    fn reassigning_creates_speakers_and_drops_empty_ones() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = three_turn_meeting(&store);
        let speakers = store.meeting_speakers(id).unwrap();
        let ids = store.segments(id).unwrap().iter().map(|segment| segment.id).collect::<Vec<_>>();

        let moves = [
            (ids[0], SpeakerTarget::New(7)),
            (ids[1], SpeakerTarget::New(7)),
            (ids[2], SpeakerTarget::Existing(speakers[1].id)),
        ];
        store.reassign_segments(id, &moves).unwrap();
        let after = store.meeting_speakers(id).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|speaker| speaker.id != speakers[0].id), "the emptied speaker is gone");
        let segments = store.segments(id).unwrap();
        assert_eq!(segments[0].speaker_id, segments[1].speaker_id);
        assert!(!segments[0].speaker_locked);
        assert_eq!(store.speaker_centroids(id).unwrap().len(), 2);
    }

    #[test]
    fn merging_speakers_of_different_meetings_is_ignored() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let first = three_turn_meeting(&store);
        let second = three_turn_meeting(&store);
        let a = store.meeting_speakers(first).unwrap()[0].id;
        let b = store.meeting_speakers(second).unwrap()[0].id;
        store.merge_speakers(a, b).unwrap();
        assert_eq!(store.meeting_speakers(first).unwrap().len(), 2);
        assert_eq!(store.meeting_speakers(second).unwrap().len(), 2);
    }

    #[test]
    fn merge_carries_the_name_and_split_keeps_the_first_group() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Team call", 1_000)).unwrap();
        let result = DiarizedTranscript {
            segments: vec![
                DiarizedSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    text: "one".into(),
                    speaker: 0,
                    overlap: false,
                    embedding: Some(vec![1.0, 0.0]),
                },
                DiarizedSegment {
                    start_ms: 1_000,
                    end_ms: 2_000,
                    text: "two".into(),
                    speaker: 0,
                    overlap: false,
                    embedding: Some(vec![1.0, 0.0]),
                },
                DiarizedSegment {
                    start_ms: 2_000,
                    end_ms: 3_000,
                    text: "three".into(),
                    speaker: 1,
                    overlap: false,
                    embedding: Some(vec![0.0, 1.0]),
                },
            ],
            centroids: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            matches: vec![None, None],
        };
        store.replace_them_segments(id, &result).unwrap();
        let speakers = store.meeting_speakers(id).unwrap();
        store.rename_speaker(speakers[0].id, Some("Anna")).unwrap();
        store.merge_speakers(speakers[0].id, speakers[1].id).unwrap();
        let remaining = store.meeting_speakers(id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, speakers[1].id);
        assert_eq!(remaining[0].name.as_deref(), Some("Anna"));
        assert_eq!(store.segments(id).unwrap().len(), 3);

        let ids = store.segments(id).unwrap().iter().map(|segment| segment.id).collect::<Vec<_>>();
        let groups = vec![vec![ids[0]], vec![ids[1], ids[2]]];
        let split = store.split_speaker(remaining[0].id, &groups).unwrap();
        assert_eq!(split.len(), 2);
        let speakers = store.meeting_speakers(id).unwrap();
        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0].name.as_deref(), Some("Anna"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retention_and_deletion_remove_voice_embeddings() {
        let dir = temp_dir();
        let store = Store::open(&dir.join("history.sqlite")).unwrap();
        let id = store.insert_meeting(&new_meeting("Old call", 1)).unwrap();
        store.set_meeting_audio(id, Some("meetings/1/them.m4a"), None).unwrap();
        store
            .replace_them_segments(
                id,
                &DiarizedTranscript {
                    segments: vec![DiarizedSegment {
                        start_ms: 0,
                        end_ms: 1_000,
                        text: "hello".into(),
                        speaker: 0,
                        overlap: false,
                        embedding: Some(vec![1.0, 0.0]),
                    }],
                    centroids: vec![vec![1.0, 0.0]],
                    matches: vec![None],
                },
            )
            .unwrap();
        assert_eq!(store.segment_embeddings(id).unwrap()[0].3.as_ref().unwrap().len(), 2);
        store.prune_meeting_audio(90 * 24 * 60 * 60 * 1000, Some(30)).unwrap();
        assert!(store.segment_embeddings(id).unwrap()[0].3.is_none());
        assert!(store.speaker_centroids(id).unwrap().is_empty());
        store.delete_meetings(&[id]).unwrap();
        assert!(store.meeting_speakers(id).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
