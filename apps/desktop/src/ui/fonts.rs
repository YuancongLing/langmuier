use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};
use skrifa::{FontRef, MetadataProvider as _};

const FONT_OVERRIDE_ENV: &str = "LANGMUIR_CJK_FONT";
const FONT_OVERRIDE_INDEX_ENV: &str = "LANGMUIR_CJK_FONT_INDEX";
const FONT_NAME: &str = "langmuir-cjk";
const REQUIRED_CJK_GLYPHS: &str = "一上下不与丢中串举义事二交产享令件任会传但位体使例信修值停储先入共关内写出分列制刷前加务动助化压原双发取叠口可后启和固图在地场型境增备复天失头始子字存学守完定实导少尾屏峰已帧常幅度开异弃式归录待心志态恢息成或户拒拟指据排接控提收故数整文断新无日旧时显暂曾有期未本机条构枚标校样桥模止正求汰法波注测淘清温溢满点特状独率环现生用电的真知码硬确磁示科程空窗立端等算类系累线终绝统缓编缺置能自航节行表见解计记设访试误请读败质路轴载辅辑输运进连送通造道遥部配采重量钟链错键长问间队际限障集频饱验";
const REQUIRED_MIXED_LABEL_GLYPHS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789RS422串口连接LVDS科学数据导出遥测CSVCPUPCB温度()[]@._-/:+%×·…℃●、（）：";

#[derive(Debug, Clone, PartialEq, Eq)]
struct FontCandidate {
    path: PathBuf,
    face_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontLoadReport {
    pub loaded_from: Option<PathBuf>,
    pub warning: Option<String>,
}

/// Installs a system CJK font as a fallback for both proportional and monospace text.
///
/// `egui`'s bundled fonts do not contain Chinese glyphs. Keeping those fonts first
/// preserves their Latin/number appearance while the system face supplies CJK glyphs.
pub fn install_cjk_fallback(context: &Context) -> FontLoadReport {
    let override_path = env::var_os(FONT_OVERRIDE_ENV).map(PathBuf::from);
    let override_index = env::var(FONT_OVERRIDE_INDEX_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let candidates = font_candidates(override_path.as_deref(), override_index);
    let mut read_errors = Vec::new();

    for candidate in candidates {
        match fs::read(&candidate.path) {
            Ok(bytes) => {
                let Ok(font) = FontRef::from_index(&bytes, candidate.face_index) else {
                    read_errors.push(format!(
                        "{}: 无效字体或 TTC 字面索引 {}",
                        candidate.path.display(),
                        candidate.face_index
                    ));
                    continue;
                };
                let charmap = font.charmap();
                if !REQUIRED_CJK_GLYPHS
                    .chars()
                    .chain(REQUIRED_MIXED_LABEL_GLYPHS.chars())
                    .all(|character| charmap.map(character).is_some())
                {
                    read_errors.push(format!(
                        "{}: 中英文混排字形覆盖不完整",
                        candidate.path.display()
                    ));
                    continue;
                }
                let mut definitions = FontDefinitions::default();
                let mut data = FontData::from_owned(bytes);
                data.index = candidate.face_index;
                definitions
                    .font_data
                    .insert(FONT_NAME.to_owned(), Arc::new(data));
                configure_font_families(&mut definitions);
                context.set_fonts(definitions);
                return FontLoadReport {
                    loaded_from: Some(candidate.path),
                    warning: None,
                };
            }
            Err(error) if candidate.path.exists() => {
                read_errors.push(format!("{}: {error}", candidate.path.display()));
            }
            Err(_) => {}
        }
    }

    let detail = if read_errors.is_empty() {
        "未找到受支持的中文系统字体".to_owned()
    } else {
        format!("中文字体读取失败：{}", read_errors.join("；"))
    };
    FontLoadReport {
        loaded_from: None,
        warning: Some(format!(
            "{detail}。可通过 {FONT_OVERRIDE_ENV} 指定字体，并用 {FONT_OVERRIDE_INDEX_ENV} 指定 TTC 字面索引"
        )),
    }
}

fn configure_font_families(definitions: &mut FontDefinitions) {
    // One proportional face renders both Latin and Chinese, so mixed labels
    // share the same baseline, cap height, and visual center.
    definitions
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, FONT_NAME.to_owned());

    // Preserve Hack for byte values and timestamps. Mixed prose is rendered
    // with the proportional family instead of combining two monospace faces.
    definitions
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push(FONT_NAME.to_owned());
}

fn font_candidates(override_path: Option<&Path>, override_index: u32) -> Vec<FontCandidate> {
    let mut candidates = Vec::new();
    if let Some(path) = override_path {
        candidates.push(FontCandidate {
            path: path.to_owned(),
            face_index: override_index,
        });
    }

    #[cfg(target_os = "macos")]
    candidates.extend([
        FontCandidate {
            path: PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"),
            face_index: 0,
        },
        FontCandidate {
            path: PathBuf::from("/System/Library/Fonts/STHeiti Light.ttc"),
            face_index: 1,
        },
        FontCandidate {
            path: PathBuf::from("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
            face_index: 0,
        },
    ]);

    #[cfg(target_os = "windows")]
    candidates.extend([
        FontCandidate {
            path: PathBuf::from(r"C:\Windows\Fonts\msyh.ttc"),
            face_index: 0,
        },
        FontCandidate {
            path: PathBuf::from(r"C:\Windows\Fonts\simhei.ttf"),
            face_index: 0,
        },
        FontCandidate {
            path: PathBuf::from(r"C:\Windows\Fonts\simsun.ttc"),
            face_index: 0,
        },
    ]);

    #[cfg(target_os = "linux")]
    candidates.extend([
        FontCandidate {
            path: PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
            face_index: 0,
        },
        FontCandidate {
            path: PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf"),
            face_index: 0,
        },
        FontCandidate {
            path: PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"),
            face_index: 0,
        },
    ]);

    candidates
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use eframe::egui::{Context, FontDefinitions, FontFamily, FontId, RawInput};

    use super::{
        FONT_NAME, REQUIRED_CJK_GLYPHS, configure_font_families, font_candidates,
        install_cjk_fallback,
    };

    #[test]
    fn explicit_font_override_is_always_first() {
        let override_path = Path::new("/tmp/langmuir-test-font.ttc");
        let candidates = font_candidates(Some(override_path), 3);
        assert_eq!(candidates[0].path, override_path);
        assert_eq!(candidates[0].face_index, 3);
    }

    #[test]
    fn platform_has_at_least_one_cjk_candidate() {
        let candidates = font_candidates(None, 0);
        assert!(!candidates.is_empty());
        assert!(candidates.iter().all(|candidate| {
            matches!(
                candidate.path.extension().and_then(|value| value.to_str()),
                Some("ttf" | "otf" | "ttc")
            )
        }));
        assert_ne!(candidates[0].path, PathBuf::new());
    }

    #[test]
    fn installed_fallback_renders_required_glyphs_in_both_families() {
        let context = Context::default();
        let report = install_cjk_fallback(&context);
        #[cfg(target_os = "macos")]
        assert!(report.loaded_from.is_some(), "{report:?}");
        if report.loaded_from.is_none() {
            return;
        }
        let _ = context.run_ui(RawInput::default(), |ui| {
            ui.ctx().fonts_mut(|fonts| {
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    let font_id = FontId::new(13.0, family);
                    assert!(fonts.has_glyphs(&font_id, REQUIRED_CJK_GLYPHS));
                }
            });
        });
    }

    #[test]
    fn mixed_labels_use_one_proportional_face() {
        let mut definitions = FontDefinitions::default();
        configure_font_families(&mut definitions);
        assert_eq!(
            definitions.families[&FontFamily::Proportional].first(),
            Some(&FONT_NAME.to_owned())
        );
        assert_eq!(
            definitions.families[&FontFamily::Monospace].last(),
            Some(&FONT_NAME.to_owned())
        );
    }
}
