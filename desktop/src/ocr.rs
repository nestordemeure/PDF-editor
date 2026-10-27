//! OCR with Tesseract (statically linked by `tesseract-rs`, called through
//! its C API) and the invisible text layer drawn over page images.

use crate::geometry::Matrix;
use crate::pixel::Raster;
use crate::writer::format_number;
use anyhow::{Context, Result, bail};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;

// Links the static Tesseract and Leptonica libraries.
use tesseract_rs as _;

unsafe extern "C" {
    fn TessBaseAPICreate() -> *mut c_void;
    fn TessBaseAPIDelete(api: *mut c_void);
    fn TessBaseAPIInit3(api: *mut c_void, datapath: *const c_char, language: *const c_char) -> c_int;
    fn TessBaseAPISetVariable(api: *mut c_void, name: *const c_char, value: *const c_char) -> c_int;
    fn TessBaseAPISetPageSegMode(api: *mut c_void, mode: c_int);
    fn TessBaseAPISetImage(api: *mut c_void, data: *const u8, w: c_int, h: c_int, bpp: c_int, bpl: c_int);
    fn TessBaseAPISetSourceResolution(api: *mut c_void, ppi: c_int);
    fn TessBaseAPIRecognize(api: *mut c_void, monitor: *mut c_void) -> c_int;
    fn TessBaseAPIClear(api: *mut c_void);
    fn TessBaseAPIGetIterator(api: *mut c_void) -> *mut c_void;
    fn TessResultIteratorDelete(it: *mut c_void);
    fn TessResultIteratorNext(it: *mut c_void, level: c_int) -> c_int;
    fn TessResultIteratorGetUTF8Text(it: *const c_void, level: c_int) -> *mut c_char;
    fn TessResultIteratorGetPageIteratorConst(it: *const c_void) -> *const c_void;
    fn TessPageIteratorIsAtBeginningOf(it: *const c_void, level: c_int) -> c_int;
    fn TessPageIteratorBoundingBox(it: *const c_void, level: c_int, l: *mut c_int, t: *mut c_int, r: *mut c_int, b: *mut c_int) -> c_int;
    fn TessPageIteratorBaseline(it: *const c_void, level: c_int, x1: *mut c_int, y1: *mut c_int, x2: *mut c_int, y2: *mut c_int) -> c_int;
    fn TessDeleteText(text: *const c_char);
}

const PSM_AUTO: c_int = 3;
const RIL_TEXTLINE: c_int = 2;
const RIL_WORD: c_int = 3;

/// Pixel box: left, top, right, bottom (y down).
pub type PixelBox = (i32, i32, i32, i32);

#[derive(Clone, Debug)]
pub struct OcrLine {
    pub bbox: PixelBox,
    /// Two points on the baseline.
    pub baseline: (i32, i32, i32, i32),
    pub words: Vec<OcrWord>,
}

#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    pub bbox: PixelBox,
}

/// Languages offered in the UI (code, name); others work when installed.
pub const COMMON_LANGUAGES: [(&str, &str); 16] = [
    ("eng", "English"),
    ("fra", "French"),
    ("deu", "German"),
    ("spa", "Spanish"),
    ("ita", "Italian"),
    ("por", "Portuguese"),
    ("nld", "Dutch"),
    ("cat", "Catalan"),
    ("pol", "Polish"),
    ("ces", "Czech"),
    ("ron", "Romanian"),
    ("swe", "Swedish"),
    ("fin", "Finnish"),
    ("dan", "Danish"),
    ("nor", "Norwegian"),
    ("lat", "Latin"),
];

const DOWNLOAD_URL: &str = "https://github.com/tesseract-ocr/tessdata_fast/raw/main";

fn system_dirs() -> Vec<PathBuf> {
    ["/usr/share/tesseract-ocr/5/tessdata", "/usr/share/tesseract-ocr/4.00/tessdata", "/usr/share/tessdata"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .collect()
}

/// The directory Tesseract reads: `$TESSDATA_PREFIX`, or this app's data
/// directory, which links system files and holds downloaded ones.
pub fn tessdata_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TESSDATA_PREFIX") {
        return PathBuf::from(dir);
    }
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    base.join("pdf-editor/tessdata")
}

fn model_file(lang: &str) -> String {
    format!("{lang}.traineddata")
}

/// True when the language can be used without a download.
pub fn is_installed(lang: &str) -> bool {
    tessdata_dir().join(model_file(lang)).exists() || system_dirs().iter().any(|d| d.join(model_file(lang)).exists())
}

/// Installed languages, from this app's directory and the system's.
pub fn available_languages() -> Vec<String> {
    let mut dirs = system_dirs();
    dirs.push(tessdata_dir());
    let mut langs: Vec<String> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".traineddata").map(str::to_owned))
        .filter(|l| l != "osd" && l.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .collect();
    langs.sort();
    langs.dedup();
    langs
}

/// Makes every language of `spec` (e.g. `eng+fra`) usable: links system
/// models into the data directory and downloads missing ones.
pub fn ensure_languages(spec: &str, progress: &dyn Fn(&str)) -> Result<()> {
    let dir = tessdata_dir();
    if std::env::var("TESSDATA_PREFIX").is_ok() {
        for lang in spec.split('+') {
            if !dir.join(model_file(lang)).exists() {
                bail!("Tesseract language '{lang}' is not in TESSDATA_PREFIX ({})", dir.display());
            }
        }
        return Ok(());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for lang in spec.split('+') {
        if lang.is_empty() || !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("invalid language code '{lang}'");
        }
        let target = dir.join(model_file(lang));
        if target.exists() {
            continue;
        }
        if let Some(system) = system_dirs().into_iter().map(|d| d.join(model_file(lang))).find(|p| p.exists()) {
            std::os::unix::fs::symlink(&system, &target).with_context(|| format!("cannot link {}", system.display()))?;
            continue;
        }
        progress(&format!("Downloading OCR language '{lang}'"));
        let url = format!("{DOWNLOAD_URL}/{}", model_file(lang));
        let mut response = ureq::get(&url).call().with_context(|| format!("cannot download {url}"))?;
        let bytes = response.body_mut().with_config().limit(200 << 20).read_to_vec().with_context(|| format!("cannot download {url}"))?;
        let partial = dir.join(format!("{}.part", model_file(lang)));
        std::fs::write(&partial, &bytes)?;
        std::fs::rename(&partial, &target)?;
    }
    Ok(())
}

/// A Tesseract instance. Not shareable between threads: keep one per thread.
pub struct Engine {
    api: *mut c_void,
    language: String,
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { TessBaseAPIDelete(self.api) };
    }
}

impl Engine {
    pub fn new(language: &str) -> Result<Engine> {
        let dir = tessdata_dir();
        for lang in language.split('+') {
            if !dir.join(format!("{lang}.traineddata")).exists() {
                bail!("Tesseract language '{lang}' is not installed in {}", dir.display());
            }
        }
        let api = unsafe { TessBaseAPICreate() };
        if api.is_null() {
            bail!("cannot create a Tesseract instance");
        }
        let engine = Engine { api, language: language.to_owned() };
        let c_dir = CString::new(dir.to_str().context("non UTF-8 tessdata path")?)?;
        let c_lang = CString::new(language)?;
        if unsafe { TessBaseAPIInit3(api, c_dir.as_ptr(), c_lang.as_ptr()) } != 0 {
            bail!("Tesseract cannot load language '{language}'");
        }
        // The C API defaults to one uniform block; find columns instead.
        unsafe { TessBaseAPISetPageSegMode(api, PSM_AUTO) };
        // Debug output would go to stderr from every thread.
        let (k, v) = (CString::new("debug_file")?, CString::new("/dev/null")?);
        unsafe { TessBaseAPISetVariable(api, k.as_ptr(), v.as_ptr()) };
        Ok(engine)
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// Recognizes an upright gray image.
    pub fn recognize(&mut self, gray: &Raster, ppi: u32) -> Result<Vec<OcrLine>> {
        if !gray.is_gray() {
            bail!("OCR needs a gray image");
        }
        let mut lines: Vec<OcrLine> = Vec::new();
        unsafe {
            TessBaseAPISetImage(self.api, gray.data.as_ptr(), gray.width as c_int, gray.height as c_int, 1, gray.width as c_int);
            TessBaseAPISetSourceResolution(self.api, ppi as c_int);
            if TessBaseAPIRecognize(self.api, std::ptr::null_mut()) != 0 {
                TessBaseAPIClear(self.api);
                bail!("Tesseract recognition failed");
            }
            let it = TessBaseAPIGetIterator(self.api);
            if !it.is_null() {
                let page = TessResultIteratorGetPageIteratorConst(it);
                loop {
                    let mut b = [0 as c_int; 4];
                    if TessPageIteratorBoundingBox(page, RIL_WORD, &mut b[0], &mut b[1], &mut b[2], &mut b[3]) != 0 {
                        if TessPageIteratorIsAtBeginningOf(page, RIL_TEXTLINE) != 0 || lines.is_empty() {
                            let mut l = [0 as c_int; 4];
                            let mut bl = [0 as c_int; 4];
                            TessPageIteratorBoundingBox(page, RIL_TEXTLINE, &mut l[0], &mut l[1], &mut l[2], &mut l[3]);
                            if TessPageIteratorBaseline(page, RIL_TEXTLINE, &mut bl[0], &mut bl[1], &mut bl[2], &mut bl[3]) == 0 {
                                bl = [l[0], l[3], l[2], l[3]];
                            }
                            lines.push(OcrLine { bbox: (l[0], l[1], l[2], l[3]), baseline: (bl[0], bl[1], bl[2], bl[3]), words: Vec::new() });
                        }
                        let text_ptr = TessResultIteratorGetUTF8Text(it, RIL_WORD);
                        if !text_ptr.is_null() {
                            let text = CStr::from_ptr(text_ptr).to_string_lossy().trim().to_owned();
                            TessDeleteText(text_ptr);
                            if !text.is_empty() {
                                lines.last_mut().unwrap().words.push(OcrWord { text, bbox: (b[0], b[1], b[2], b[3]) });
                            }
                        }
                    }
                    if TessResultIteratorNext(it, RIL_WORD) == 0 {
                        break;
                    }
                }
                TessResultIteratorDelete(it);
            }
            TessBaseAPIClear(self.api);
        }
        lines.retain(|l| !l.words.is_empty());
        Ok(lines)
    }
}

/// Name of the text-layer font in page resources.
pub const FONT_NAME: &str = "OCR";

/// Content stream operators drawing invisible text (render mode 3) for
/// lines found on an upright image. `px_to_page` maps image pixels (y down)
/// to page points (y up).
pub fn text_layer(lines: &[OcrLine], px_to_page: Matrix) -> Vec<u8> {
    let sx = px_to_page.a.abs();
    let sy = px_to_page.d.abs();
    let f = format_number;
    let mut out = String::from("BT 3 Tr\n");
    for line in lines {
        let (bx1, by1, bx2, by2) = line.baseline;
        let slope = if bx2 != bx1 { (by2 - by1) as f64 / (bx2 - bx1) as f64 } else { 0.0 };
        let angle = (-(by2 - by1) as f64 * sy).atan2(((bx2 - bx1) as f64 * sx).max(1e-9));
        let (cos, sin) = (angle.cos(), angle.sin());
        // Glyphs span the em above the origin: the origin sits on the
        // line's bottom edge and the em is the line height.
        let descent = ((line.bbox.3 - by1) as f64).max(0.0);
        let size = ((line.bbox.3 - line.bbox.1) as f64 * sy).max(1.0);
        out.push_str(&format!("/{FONT_NAME} {} Tf\n", f(size)));
        for (i, word) in line.words.iter().enumerate() {
            let next = line.words.get(i + 1);
            let mut text = word.text.clone();
            if next.is_some() {
                text.push(' ');
            }
            let units: Vec<u16> = text.encode_utf16().collect();
            let x0 = word.bbox.0 as f64;
            let base_y = by1 as f64 + slope * (x0 - bx1 as f64) + descent;
            let span_px = match next {
                Some(n) => n.bbox.0 - word.bbox.0,
                None => word.bbox.2 - word.bbox.0,
            };
            let natural = units.len() as f64 * 0.5 * size;
            let stretch = (100.0 * span_px.max(1) as f64 * sx / natural).clamp(1.0, 10000.0);
            let (x, y) = px_to_page.apply(x0, base_y);
            out.push_str(&format!("{} {} {} {} {} {} Tm {} Tz <", f(cos), f(sin), f(-sin), f(cos), f(x), f(y), f(stretch)));
            for u in units {
                out.push_str(&format!("{u:04X}"));
            }
            out.push_str("> Tj\n");
        }
    }
    out.push_str("ET\n");
    out.into_bytes()
}

pub const GLYPHLESS_FONT: &[u8] = include_bytes!("../assets/glyphless.ttf");

pub const TO_UNICODE_CMAP: &str = "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange\n\
endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
