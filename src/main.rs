use clap::Parser;
use colored::*;
use encoding_rs::WINDOWS_1251;
use lofty::config::{ParseOptions, WriteOptions};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::TagExt;
use phf::{phf_set, Set};
use std::fmt::Debug;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

static AUDIO_EXTENSIONS: Set<&'static str> = phf_set! {"mp3", "flac", "m4a", "mp4", "ogg", "wav"};
static TEXT_EXTENSIONS: Set<&'static str> = phf_set! {"cue"};
static LATIN_DIACRITICS: Set<char> = phf_set! {
'ä', 'ö', 'ü', 'ß', 'Ä', 'Ö', 'Ü', 'é', 'è', 'ê', 'ë', 'á', 'à', 'â', 'å', 'í', 'ì', 'î', 'ó',
'ò', 'ô', 'ú', 'ù', 'û'};

const WEIGHT_CYR: f64 = 1.0;
const WEIGHT_DIACRITICS: f64 = 0.8;

/// Простая утилита для исправления кириллических кракозябр в тегах музыкальных и .cue файлов
#[derive(Parser, Debug)]
#[command(
    version,
    about = "Утилита для исправления кириллических кракозябр кодировки cp1251 в тегах музыкальных и .cue файлов",
    arg_required_else_help = true
)]
struct Args {
    /// Путь к папке с музыкой
    path: PathBuf,

    /// Не создавать .bak файлы (по умолчанию создаются)
    #[arg(long)]
    no_backup: bool,

    /// Принудительно считать все .cue файлами в cp1251 (без попыток угадать)
    #[arg(long)]
    force_cp1251_cue: bool,

    /// Отрегулировать порог определения кириллицы
    #[arg(long, default_value_t = 0.2)]
    cyr_threshold: f64,
}

struct BackupManager {
    no_backup: bool,
}

fn main() {
    let args = Args::parse();

    if !args.path.exists() {
        eprintln!(
            "{}: путь не найден: {}",
            "Ошибка".red(),
            args.path.display()
        );
        std::process::exit(1);
    }

    println!(
        "{} {}",
        "Старт обработки каталога:".green().bold(),
        args.path.display()
    );

    let mut count_fixed = 0usize;
    let bm = BackupManager {
        no_backup: args.no_backup,
    };

    for entry in WalkDir::new(&args.path).follow_links(true) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                eprintln!("{}: {}", "Ошибка обхода".red(), err);
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
            continue;
        };

        let ext = ext.to_lowercase();

        if TEXT_EXTENSIONS.contains(ext.as_str()) && process_cue(path, &bm, args.force_cp1251_cue) {
            println!("{:<6} {}", "[CUE]".magenta(), path.display());
            count_fixed += 1;
        } else if AUDIO_EXTENSIONS.contains(ext.as_str())
            && process_audio(path, &bm, args.cyr_threshold)
        {
            println!(
                "{:<6} {}",
                format!("[{}]", ext.to_uppercase()).bright_blue(),
                path.display()
            );
            count_fixed += 1;
        }
    }

    println!(
        "{} {} файлов было исправлено.",
        "Готово!".green().bold(),
        count_fixed.to_string().bold()
    );
}

// fn has_cyrillic(s: &str) -> bool {
//     s.chars()
//         .any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
// }

fn is_cyrillic(c: &char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(c)
}

fn cyrillic_count(s: &str) -> usize {
    s.chars().filter(is_cyrillic).count()
}

fn latin_diacritics_count(s: &str) -> usize {
    s.chars().filter(|c| LATIN_DIACRITICS.contains(c)).count()
}

/// "Ëüâèöà ðîêà" -> "Львица рока"
fn fix_mojibake(text: &str, cyr_threshold: f64) -> Option<String> {
    if cyrillic_count(text) > 0 {
        return None;
    }

    let (latin1_bytes, _, _) = WINDOWS_1251.encode(text);
    let (decoded, _, _) = WINDOWS_1251.decode(&latin1_bytes);
    let decoded_str = decoded.trim().to_string();

    let len = decoded_str.chars().count() as f64;

    let cyr_ratio = cyrillic_count(&decoded_str) as f64 / len;
    let diacritics_ratio = latin_diacritics_count(text) as f64 / len;
    let score = WEIGHT_CYR * cyr_ratio - WEIGHT_DIACRITICS * diacritics_ratio;

    if score > cyr_threshold {
        Some(decoded_str)
    } else {
        None
    }
}

/// Обработка .cue файла: читаем cp1251 -> пишем utf-8
fn process_cue(path: &Path, backup_manager: &BackupManager, force_cp1251: bool) -> bool {
    let mut raw = Vec::new();
    if let Err(e) = File::open(path).and_then(|mut f| f.read_to_end(&mut raw)) {
        eprintln!("{} чтения {}: {e}", "Ошибка".red(), path.display());
        return false;
    }

    // Пробуем определить кодировку:
    // если force_cp1251 — просто cp1251;
    // иначе: пробуем cp1251, если неудачно — пробуем utf-8, иначе оставляем как есть.
    let content = if force_cp1251 {
        let (decoded, _, had_errors) = WINDOWS_1251.decode(&raw);
        if had_errors {
            eprintln!(
                "{}: не удалось полностью декодировать {} как cp1251",
                "Внимание".yellow(),
                path.display()
            );
        }
        decoded.to_string()
    } else {
        // 1) пробуем utf-8
        if String::from_utf8(raw.clone()).is_ok() {
            // если текст нормальный, просто ничего не делаем
            return false;
        } else {
            // 2) пробуем cp1251
            let (decoded, _, _) = WINDOWS_1251.decode(&raw);
            decoded.to_string()
        }
    };

    if let Err(e) = backup_manager.backup_file(path) {
        eprintln!("{e}");
        return false;
    }

    if let Err(e) = fs::write(path, content.as_bytes()) {
        eprintln!("{} записи {}: {e}", "Ошибка".red(), path.display());
        return false;
    }

    println!("  {}", "→ .cue сохранён в UTF-8".green());
    true
}

/// Обработка аудио-файла через lofty
fn process_audio(path: &Path, backup_manager: &BackupManager, cyr_threshold: f64) -> bool {
    let parse_opts = ParseOptions::new();
    let tagged_file = match Probe::open(path).and_then(|p| p.options(parse_opts).read()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{} чтения тегов {}: {e}", "Ошибка".red(), path.display());
            return false;
        }
    };

    let mut tag = match tagged_file.primary_tag() {
        Some(t) => t.to_owned(),
        None => match tagged_file.first_tag() {
            Some(t) => t.to_owned(),
            None => return false,
        },
    };

    let mut fixes: Vec<(ItemKey, String)> = Vec::new();

    for item in tag.items() {
        if let Some(text) = item.value().text()
            && let Some(fixed) = fix_mojibake(text, cyr_threshold)
        {
            println!(
                "  {} {:?}: '{}' -> '{}'",
                "FIX".cyan(),
                item.key(),
                text,
                fixed
            );
            fixes.push((item.key().clone(), fixed));
        }
    }

    if fixes.is_empty() {
        return false;
    }
    for (key, fixed) in fixes {
        tag.insert_text(key, fixed);
    }

    if let Err(e) = backup_manager.backup_file(path) {
        eprintln!("{e}");
        return false;
    }

    if let Err(e) = tag.save_to_path(path, WriteOptions::default()) {
        eprintln!(
            "{} сохранения тегов {}: {e}",
            "Ошибка".red(),
            path.display()
        );
        return false;
    }

    println!("  {}", "→ теги обновлены".green());
    true
}

impl BackupManager {
    fn create_backup(&self, path: &Path) -> std::io::Result<()> {
        let file_name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Не удалось получить имя файла",
            )
        })?;

        let backup_path = path.with_file_name(format!("{}.bak", file_name.to_string_lossy()));

        fs::copy(path, backup_path)?;
        Ok(())
    }

    pub fn backup_file(&self, path: &Path) -> std::io::Result<()> {
        if self.no_backup {
            return Ok(());
        }
        self.create_backup(path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "{} при создании бэкапа {}: {e}",
                    "Ошибка".red(),
                    path.display()
                ),
            )
        })
    }
}
