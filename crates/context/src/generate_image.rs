//! JS: generate-image.mjs -> `impeccable generate-image`

use crate::jsp;
use crate::util::{iso_now, json_pretty, node_read_error, utf16_len, Env};
use impeccable_common::Io;
use serde_json::{Map, Value};
use std::io::Write;

pub const DEFAULT_MODEL: &str = "gpt-image-2.5-flare";

/// Nano Banana Pro: the Gemini image model that renders UI text most legibly,
/// which is what a comp needs. `IMPECCABLE_GEMINI_IMAGE_MODEL` or `--model`
/// overrides it (for example `gemini-3.1-flash-image`, cheaper per image).
pub const GEMINI_DEFAULT_MODEL: &str = "gemini-3-pro-image";

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The image API a real-mode run calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    OpenAi,
    Gemini,
}

fn env_value(env: &Env, name: &str) -> Option<String> {
    env.get(name).filter(|v| !v.trim().is_empty()).cloned()
}

/// Which backend real mode uses and the key it sends. `IMPECCABLE_IMAGE_PROVIDER`
/// (`openai` or `gemini`) pins one; otherwise an OpenAI key wins, so an
/// environment that had one keeps behaving as before, and a Gemini key is used
/// when it is the only one. `Ok(None)` means no key at all. `context` calls this
/// too, so IMAGE_GEN_AVAILABLE always names the backend this verb would use.
pub fn resolve_backend(env: &Env) -> Result<Option<(Backend, String)>, String> {
    let openai = env_value(env, "OPENAI_API_KEY");
    let gemini = env_value(env, "GEMINI_API_KEY");
    match env_value(env, "IMPECCABLE_IMAGE_PROVIDER").map(|p| p.trim().to_ascii_lowercase()).as_deref() {
        None => Ok(openai.map(|k| (Backend::OpenAi, k)).or(gemini.map(|k| (Backend::Gemini, k)))),
        Some("openai") => openai.map(|k| Some((Backend::OpenAi, k))).ok_or_else(|| "IMPECCABLE_IMAGE_PROVIDER is openai but OPENAI_API_KEY is not set".into()),
        Some("gemini") => gemini.map(|k| Some((Backend::Gemini, k))).ok_or_else(|| "IMPECCABLE_IMAGE_PROVIDER is gemini but GEMINI_API_KEY is not set".into()),
        Some(other) => Err(format!("IMPECCABLE_IMAGE_PROVIDER must be openai or gemini, not \"{other}\"")),
    }
}

/// The Gemini model a run uses when `--model` is not given.
pub fn gemini_model(env: &Env) -> String {
    env_value(env, "IMPECCABLE_GEMINI_IMAGE_MODEL").map(|m| m.trim().to_string()).unwrap_or_else(|| GEMINI_DEFAULT_MODEL.into())
}

fn arg(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == &format!("--{}", name))?;
    let v = args.get(i + 1)?;
    if !v.is_empty() && !v.starts_with("--") {
        Some(v.clone())
    } else {
        None
    }
}

fn hash32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for cu in s.encode_utf16() {
        h ^= cu as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

fn hsl_to_rgb(h_deg: f64, s: f64, l: f64) -> [u8; 3] {
    let h = ((h_deg % 360.0) + 360.0) % 360.0 / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |t: f64| -> f64 {
        let mut tt = t;
        if tt < 0.0 {
            tt += 1.0;
        }
        if tt > 1.0 {
            tt -= 1.0;
        }
        if tt < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * tt;
        }
        if tt < 1.0 / 2.0 {
            return q;
        }
        if tt < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - tt) * 6.0;
        }
        p
    };
    let round = |c: f64| -> u8 { js_round(c * 255.0) as u8 };
    [round(hue(h + 1.0 / 3.0)), round(hue(h)), round(hue(h - 1.0 / 3.0))]
}

/// Math.round: half up toward +inf
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

fn to_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn palette(prompt: &str) -> Vec<[u8; 3]> {
    let h = hash32(prompt);
    let base = (h % 360) as f64;
    let bands = 2 + ((h >> 9) % 2) as usize;
    let spread = (40 + (h >> 3) % 120) as f64;
    let mut out = Vec::new();
    for i in 0..bands {
        let hue = base + i as f64 * spread;
        let light = 0.32 + ((h >> (i * 5)) % 40) as f64 / 100.0;
        out.push(hsl_to_rgb(hue, 0.55, light));
    }
    out
}

fn collapse_ws(s: &str) -> String {
    // .replace(/\s+/g, ' ').trim()
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() || c == '\u{FEFF}' {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            in_ws = false;
            out.push(c);
        }
    }
    crate::util::js_trim(&out).to_string()
}

fn num(v: f64) -> String {
    crate::util::js_number_to_string(v)
}

fn svg_fake(prompt: &str, w: f64, h: f64) -> String {
    let colors: Vec<String> = palette(prompt).into_iter().map(to_hex).collect();
    let n = colors.len();
    let stops: String = colors
        .iter()
        .enumerate()
        .map(|(i, c)| format!("<stop offset=\"{}%\" stop-color=\"{}\"/>", num(js_round(i as f64 / (n as f64 - 1.0) * 100.0)), c))
        .collect();
    let per_line = (12.0f64).max((w / 26.0).floor()) as usize;
    let words: Vec<&str> = {
        let collapsed = collapse_ws(prompt);
        // split(' ') on the collapsed string; leak for lifetime simplicity
        Box::leak(collapsed.into_boxed_str()).split(' ').collect()
    };
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in words {
        let candidate = crate::util::js_trim(&format!("{} {}", cur, word)).to_string();
        if utf16_len(&candidate) > per_line {
            if !cur.is_empty() {
                lines.push(cur.clone());
            }
            cur = word.to_string();
        } else {
            cur = candidate;
        }
        if lines.len() >= 10 {
            break;
        }
    }
    if !cur.is_empty() && lines.len() < 11 {
        lines.push(cur);
    }
    let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let font_size = js_round(w / 24.0);
    let start_y = h / 2.0 - ((lines.len() as f64 - 1.0) * font_size * 1.3) / 2.0;
    let text: String = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            format!(
                "<text x=\"{}\" y=\"{}\" font-family=\"Helvetica, Arial, sans-serif\" font-size=\"{}\" fill=\"#ffffff\" text-anchor=\"middle\" dominant-baseline=\"middle\">{}</text>",
                num(w / 2.0),
                num(js_round(start_y + i as f64 * font_size * 1.3)),
                num(font_size),
                escape(line)
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n  <defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"1\">{stops}</linearGradient></defs>\n  <rect width=\"{w}\" height=\"{h}\" fill=\"url(#g)\"/>\n  <rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"#000000\" fill-opacity=\"0.22\"/>\n  {text}\n  <rect x=\"{x1}\" y=\"{y1}\" width=\"{bw}\" height=\"{bh}\" fill=\"#000000\" fill-opacity=\"0.55\"/>\n  <text x=\"{tx}\" y=\"{ty}\" font-family=\"Helvetica, Arial, sans-serif\" font-size=\"{fs}\" letter-spacing=\"2\" fill=\"#ffffff\" text-anchor=\"middle\" dominant-baseline=\"middle\">SYNTHETIC COMP</text>\n</svg>\n",
        w = num(w),
        h = num(h),
        stops = stops,
        text = text,
        x1 = num(w - js_round(w / 4.2)),
        y1 = num(h - js_round(h / 16.0)),
        bw = num(js_round(w / 4.2)),
        bh = num(js_round(h / 16.0)),
        tx = num(w - js_round(w / 8.4)),
        ty = num(h - js_round(h / 32.0)),
        fs = num(js_round(w / 60.0)),
    )
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffffffffu32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 == 1 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    c ^ 0xffffffff
}

fn png_chunk(ty: &[u8], data: &[u8]) -> Vec<u8> {
    let mut body = ty.to_vec();
    body.extend_from_slice(data);
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

fn png_fake(prompt: &str, w: usize, h: usize) -> Vec<u8> {
    png_fake_background(prompt, w, h, false)
}

fn png_fake_background(prompt: &str, w: usize, h: usize, transparent: bool) -> Vec<u8> {
    let colors = palette(prompt);
    let band_h = (h as f64 / colors.len() as f64).ceil() as usize;
    let channels = if transparent { 4 } else { 3 };
    let stride = w * channels;
    let mut raw = vec![0u8; h * (stride + 1)];
    for y in 0..h {
        let row = y * (stride + 1);
        raw[row] = 0;
        let idx = (colors.len() - 1).min(if band_h == 0 { 0 } else { y / band_h });
        let [r, g, b] = colors[idx];
        for x in 0..w {
            let p = row + 1 + x * channels;
            raw[p] = r;
            raw[p + 1] = g;
            raw[p + 2] = b;
            if transparent {
                raw[p + 3] = if x < w / 8 || x >= w - w / 8 || y < h / 8 || y >= h - h / 8 {
                    0
                } else {
                    255
                };
            }
        }
    }
    let mut ihdr = vec![0u8; 13];
    ihdr[..4].copy_from_slice(&(w as u32).to_be_bytes());
    ihdr[4..8].copy_from_slice(&(h as u32).to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = if transparent { 6 } else { 2 };
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
    let _ = enc.write_all(&raw);
    let idat = enc.finish().unwrap_or_default();
    let mut text = b"Comment".to_vec();
    text.push(0);
    // latin1: chars > 0xff become their low byte in Node's 'latin1' encoding
    for c in format!("SYNTHETIC COMP: {}", collapse_ws(prompt)).encode_utf16() {
        text.push((c & 0xff) as u8);
    }
    let mut out = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    out.extend(png_chunk(b"IHDR", &ihdr));
    out.extend(png_chunk(b"tEXt", &text));
    out.extend(png_chunk(b"IDAT", &idat));
    out.extend(png_chunk(b"IEND", &[]));
    out
}

fn parse_size(s: &str) -> (usize, usize) {
    if let Some((a, b)) = s.split_once('x') {
        if !a.is_empty() && !b.is_empty() && a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit()) {
            return (a.parse().unwrap_or(1536), b.parse().unwrap_or(1024));
        }
    }
    (1536, 1024)
}

/// How long one `--ref <url>` download may take, connect to last byte.
const REF_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The largest reference image a URL may hand back.
const MAX_IMAGE_BYTES: u64 = 30 * 1024 * 1024;

/// Whether a `--ref` value (or a card image) is a URL rather than a path.
pub(crate) fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// The URL without its query and fragment.
fn url_path(url: &str) -> &str {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    &url[..end]
}

fn sniffed_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// Downloads one png, webp, or jpeg image. The bytes' signature decides the
/// type and is required: a `200` HTML page behind a `.webp` URL or an image
/// Content-Type (a soft 404, an auth wall, a proxy interstitial) is refused
/// rather than uploaded or kept as an image. `redirects` is how many the
/// request may follow. The error is a short reason with no OS-specific text;
/// the caller names the URL.
pub(crate) fn fetch_image(url: &str, timeout: std::time::Duration, redirects: u32) -> Result<(Vec<u8>, &'static str), String> {
    use std::io::Read;
    // One deadline for the whole fetch, redirects included: each hop gets
    // only the time that is left.
    let started = std::time::Instant::now();
    let response = match crate::http::get_following(url, redirects, |b| {
        let left = timeout.saturating_sub(started.elapsed()).max(std::time::Duration::from_millis(1));
        b.timeout_connect(left).timeout(left)
    }) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {}", code)),
        Err(_) => return Err("the request failed".into()),
    };
    if !(200..300).contains(&response.status()) {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut bytes = Vec::new();
    if response.into_reader().take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes).is_err() {
        return Err("the response was cut short".into());
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("the image is larger than 30 MB".into());
    }
    if bytes.is_empty() {
        return Err("the response was empty".into());
    }
    sniffed_type(&bytes).map(|t| (bytes, t)).ok_or_else(|| "the response is not a png, webp, or jpeg image".into())
}

/// The multipart file name for a URL ref: its last path segment.
fn url_file_name(url: &str) -> String {
    let rest = url_path(url).split_once("://").map(|(_, r)| r).unwrap_or("");
    let segment = rest.split_once('/').map(|(_, p)| p.rsplit('/').next().unwrap_or("")).unwrap_or("");
    let name: String = segment.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect();
    if name.is_empty() { "ref".to_string() } else { name }
}

/// Reads one `--ref`: a URL is downloaded and its bytes' signature names the
/// type, a path is read from disk and its extension names the type. On failure
/// it prints why and returns the exit code; nothing has been generated yet.
fn load_ref(io: &mut Io, cwd: &str, r: &str) -> Result<(Vec<u8>, &'static str, String), i32> {
    // A URL ref (a catalog world's card image, say) is downloaded and
    // attached like a file; nothing is written when one fails.
    if is_url(r) {
        return match fetch_image(r, REF_DOWNLOAD_TIMEOUT, 5) {
            Ok((bytes, ty)) => Ok((bytes, ty, url_file_name(r))),
            Err(reason) => {
                io.err(&format!("generate-image: could not download --ref {}: {}; nothing was generated.\n", r, reason));
                Err(1)
            }
        };
    }
    let bytes = match std::fs::read(jsp::resolve(cwd, &[r])) {
        Ok(b) => b,
        Err(e) => {
            io.err(&format!("Error: {}\n", node_read_error(r, &e)));
            return Err(1);
        }
    };
    let ty = if r.ends_with(".png") {
        "image/png"
    } else if r.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    };
    Ok((bytes, ty, r.rsplit('/').next().unwrap_or(r).to_string()))
}

/// Best effort: writes the prompt into the image itself, as `embed-prompt` does.
fn embed(io: &mut Io, out: &str, prompt: &str) -> bool {
    // JS-PARITY: generate-image.mjs#676 reports whether the embed actually
    // succeeded. The install-path-with-spaces half of #676 is a JS-only
    // subprocess concern (fileURLToPath vs URL.pathname); the engine embeds
    // in-process, so only the success tracking and message carry over here.
    let mut sub_io = Io::captured("", io.cwd.clone(), io.env.clone()).0;
    let embedded = crate::embed_prompt::run(&[out.to_string(), "--prompt".to_string(), prompt.to_string()], &mut sub_io) == 0;
    if !embedded {
        io.err("generate-image: failed to embed prompt in the image\n");
    }
    embedded
}

/// How long one Gemini generation may take, connect to last byte. Nano Banana
/// Pro thinks before it draws, so this is generous; it exists so a stalled
/// connection cannot hang the agent's shell forever.
const GEMINI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The aspect ratios Gemini image models accept in `imageConfig.aspectRatio`.
const GEMINI_ASPECTS: [(&str, f64); 10] = [
    ("1:1", 1.0),
    ("2:3", 2.0 / 3.0),
    ("3:2", 3.0 / 2.0),
    ("3:4", 3.0 / 4.0),
    ("4:3", 4.0 / 3.0),
    ("4:5", 4.0 / 5.0),
    ("5:4", 5.0 / 4.0),
    ("9:16", 9.0 / 16.0),
    ("16:9", 16.0 / 9.0),
    ("21:9", 21.0 / 9.0),
];

/// Gemini takes an aspect ratio rather than pixels: the supported ratio
/// nearest the requested `--size` on a log scale (first listed wins a tie).
fn nearest_aspect(w: usize, h: usize) -> &'static str {
    let target = (w.max(1) as f64 / h.max(1) as f64).ln();
    let distance = |ratio: f64| (ratio.ln() - target).abs();
    GEMINI_ASPECTS.iter().min_by(|a, b| distance(a.1).total_cmp(&distance(b.1))).map(|a| a.0).unwrap_or("1:1")
}

/// `imageConfig.imageSize` for models that take it (Gemini 3 and later): the
/// smallest tier whose long side covers the requested one, and at least 2K for
/// `--quality high`, which plates ask for. Gemini 2.x image models reject the
/// field, so they get none and draw at their fixed size.
fn gemini_image_size(model: &str, w: usize, h: usize, quality: &str) -> Option<&'static str> {
    if model.starts_with("gemini-2.") || model.starts_with("gemini-1.") {
        return None;
    }
    let long = w.max(h);
    Some(if long > 2048 {
        "4K"
    } else if long > 1024 || quality == "high" {
        "2K"
    } else {
        "1K"
    })
}

/// A model name that is safe to put in the request path.
fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 120 && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The image a `generateContent` response carries: the last part that is not
/// a thought (Nano Banana Pro may send interim "thought" images before the
/// final one), else the last image of any kind. With none, the reason: a block
/// reason, a finish reason other than STOP, and any text the model wrote
/// instead of drawing.
fn gemini_response_image(json: &Value) -> Result<Vec<u8>, String> {
    let candidate = json.get("candidates").and_then(|c| c.get(0));
    let parts: Vec<Value> = candidate.and_then(|c| c.get("content")).and_then(|c| c.get("parts")).and_then(|p| p.as_array()).cloned().unwrap_or_default();
    let thought = |p: &Value| p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
    let image_of = |p: &Value| -> Option<String> {
        let inline = p.get("inlineData").or_else(|| p.get("inline_data"))?;
        inline.get("data")?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    };
    if let Some(data) = parts.iter().rev().filter(|p| !thought(p)).find_map(image_of).or_else(|| parts.iter().rev().find_map(image_of)) {
        return Ok(base64_decode(&data));
    }
    let mut why = Vec::new();
    if let Some(reason) = json.get("promptFeedback").and_then(|f| f.get("blockReason")).and_then(|r| r.as_str()) {
        why.push(format!("the prompt was blocked ({reason})"));
    }
    if let Some(reason) = candidate.and_then(|c| c.get("finishReason")).and_then(|r| r.as_str()).filter(|r| *r != "STOP") {
        why.push(format!("finish reason {reason}"));
    }
    let text = collapse_ws(&parts.iter().filter(|p| !thought(p)).filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join(" "));
    if !text.is_empty() {
        why.push(format!("the model answered in text: {}", text.chars().take(300).collect::<String>()));
    }
    Err(if why.is_empty() { "the response held no image".into() } else { why.join("; ") })
}

/// Width and height of an image's bytes: a PNG's header, else a full decode.
fn image_dims(bytes: &[u8]) -> Option<(usize, usize)> {
    if impeccable_comp::png_io::is_png(bytes) && bytes.len() >= 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?) as usize;
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?) as usize;
        return Some((w, h));
    }
    impeccable_comp::png_io::decode_source(bytes).ok().map(|img| (img.width, img.height))
}

struct GeminiJob<'a> {
    prompt: &'a str,
    out: &'a str,
    size: &'a str,
    quality: &'a str,
    model: &'a str,
    refs: &'a [String],
    background: Option<&'a str>,
}

/// Real mode against the Gemini API: one `generateContent` call with the refs
/// as inline images ahead of the prompt, the size expressed as an aspect ratio
/// and an output tier. A `.png` out is always PNG on disk (a JPEG or WebP
/// answer is converted), so `embed-prompt`, `comp-spec`, and the plates gate
/// read it like an OpenAI image.
fn run_gemini(io: &mut Io, key: &str, base: &str, job: &GeminiJob) -> i32 {
    let cwd = io.cwd.to_string_lossy().into_owned();
    let model = job.model.strip_prefix("models/").unwrap_or(job.model);
    if !valid_model(model) {
        io.err(&format!("generate-image: --model {:?} is not a Gemini model name.\n", job.model));
        return 1;
    }
    let out_lower = job.out.to_ascii_lowercase();
    let png_out = out_lower.ends_with(".png");
    let wanted = if png_out {
        "image/png"
    } else if out_lower.ends_with(".jpg") || out_lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if out_lower.ends_with(".webp") {
        "image/webp"
    } else {
        io.err("generate-image: the Gemini backend writes raster images; use a .png --out.\n");
        return 1;
    };
    let (w, h) = parse_size(job.size);
    let aspect = nearest_aspect(w, h);
    let image_size = gemini_image_size(model, w, h, job.quality);
    let mut parts: Vec<Value> = Vec::new();
    for r in job.refs {
        let (bytes, ty, _) = match load_ref(io, &cwd, r) {
            Ok(loaded) => loaded,
            Err(code) => return code,
        };
        let mime = sniffed_type(&bytes).unwrap_or(ty);
        parts.push(serde_json::json!({ "inlineData": { "mimeType": mime, "data": base64_encode(&bytes) } }));
    }
    parts.push(serde_json::json!({ "text": job.prompt }));
    let mut image_config = Map::new();
    image_config.insert("aspectRatio".into(), Value::String(aspect.into()));
    if let Some(tier) = image_size {
        image_config.insert("imageSize".into(), Value::String(tier.into()));
    }
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": parts }],
        "generationConfig": { "responseModalities": ["TEXT", "IMAGE"], "imageConfig": image_config },
    });
    let agent = crate::http::agent_builder(base).timeout(GEMINI_TIMEOUT).build();
    let response = agent
        .post(&format!("{base}/models/{model}:generateContent"))
        .set("x-goog-api-key", key)
        .set("content-type", "application/json")
        .send_string(&body.to_string());
    let (status, text) = match response {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => {
            io.err(&format!("generate-image: the Gemini request failed: {}\n", e));
            return 1;
        }
    };
    if !(200..300).contains(&status) {
        let snippet: String = text.chars().take(300).collect();
        let hint = match status {
            404 => format!(" (no model {model} for this key: set IMPECCABLE_GEMINI_IMAGE_MODEL or pass --model with an image model the key can use)"),
            429 => " (quota exhausted: image models may need a billing-enabled project, which Google AI plan credits can fund)".to_string(),
            _ => String::new(),
        };
        io.err(&format!("generate-image: API error {}: {}{}\n", status, snippet, hint));
        return 1;
    }
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut bytes = match gemini_response_image(&json) {
        Ok(b) => b,
        Err(reason) => {
            io.err(&format!("generate-image: no image in response: {}\n", reason));
            return 1;
        }
    };
    let got = sniffed_type(&bytes);
    if got != Some(wanted) {
        if !png_out {
            io.err(&format!("generate-image: Gemini returned {}, not what --out names; use a .png --out, which converts.\n", got.unwrap_or("an unrecognized format")));
            return 1;
        }
        match impeccable_comp::png_io::decode_source(&bytes).and_then(|img| impeccable_comp::png_io::encode_png(&img, &[])) {
            Ok(png) => bytes = png,
            Err(e) => {
                io.err(&format!("generate-image: the Gemini image could not be converted to PNG: {}\n", e));
                return 1;
            }
        }
    }
    if let Err(e) = std::fs::write(jsp::resolve(&cwd, &[job.out]), &bytes) {
        io.err(&format!("Error: {}\n", node_read_error(job.out, &e)));
        return 1;
    }
    let embedded = embed(io, job.out, job.prompt);
    let mut m = Map::new();
    m.insert("prompt".into(), Value::String(job.prompt.to_string()));
    m.insert("createdAt".into(), Value::String(iso_now()));
    m.insert("tool".into(), Value::String("impeccable generate-image".into()));
    m.insert("provider".into(), Value::String("gemini".into()));
    m.insert("model".into(), Value::String(model.to_string()));
    m.insert("aspectRatio".into(), Value::String(aspect.into()));
    if let Some(tier) = image_size {
        m.insert("imageSize".into(), Value::String(tier.into()));
    }
    if let Some(background) = job.background {
        m.insert("background".into(), Value::String(background.into()));
    }
    if !job.refs.is_empty() {
        m.insert("refs".into(), Value::Array(job.refs.iter().cloned().map(Value::String).collect()));
    }
    let _ = std::fs::write(jsp::resolve(&cwd, &[&format!("{}.json", job.out)]), json_pretty(&Value::Object(m)));
    note_generated_slot(io, job.out);
    let dims = image_dims(&bytes).map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "size unknown".into());
    let tier = image_size.map(|t| format!(" at {t}")).unwrap_or_default();
    io.out(&format!(
        "IMAGE: {} ({}, {}{}, {}, billed to your Gemini API key); {} at {}.json\n",
        job.out,
        dims,
        aspect,
        tier,
        model,
        if embedded { "prompt embedded + sidecar" } else { "sidecar" },
        job.out
    ));
    0
}

pub fn run(args: &[String], io: &mut Io) -> i32 {
    let api_base = api_base(&io.env);
    let gemini_base = gemini_api_base(&io.env);
    run_with_bases(args, io, &api_base, &gemini_base)
}

/// The Gemini API base (`/v1beta` included) that `/models/<model>:generateContent`
/// hangs off. `IMPECCABLE_GEMINI_BASE_URL` points it at a proxy or a test server.
fn gemini_api_base(env: &Env) -> String {
    env.get("IMPECCABLE_GEMINI_BASE_URL")
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| GEMINI_API_BASE.into())
}

/// OPENAI_BASE_URL is the variable the OpenAI SDKs read for a proxy or compatible endpoint, with
/// the same meaning: the base that `/images/...` hangs off, `/v1` included.
fn api_base(env: &Env) -> String {
    env.get("OPENAI_BASE_URL")
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "https://api.openai.com/v1".into())
}

#[cfg(test)]
fn run_with_api_base(args: &[String], io: &mut Io, api_base: &str) -> i32 {
    let gemini_base = gemini_api_base(&io.env);
    run_with_bases(args, io, api_base, &gemini_base)
}

fn run_with_bases(args: &[String], io: &mut Io, api_base: &str, gemini_base: &str) -> i32 {
    let background = arg(args, "background");
    if args.iter().any(|a| a == "--background") && !matches!(background.as_deref(), Some("transparent" | "opaque" | "auto")) {
        io.err("generate-image: --background must be transparent, opaque, or auto.\n");
        return 1;
    }
    let transparent = background.as_deref() == Some("transparent");
    if background.is_some() && arg(args, "out").is_some_and(|out| !out.to_ascii_lowercase().ends_with(".png")) {
        io.err("generate-image: --background requires a .png --out path.\n");
        return 1;
    }
    let cwd = io.cwd.to_string_lossy().into_owned();
    let env: Env = io.env.clone();
    let abs = |p: &str| jsp::resolve(&cwd, &[p]);
    let read_prompt_file = |io: &mut Io, pf: &str| -> Result<String, i32> {
        match std::fs::read(abs(pf)) {
            Ok(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
            Err(e) => {
                io.err(&format!("Error: {}\n", node_read_error(pf, &e)));
                Err(1)
            }
        }
    };
    if env.get("IMPECCABLE_IMAGE_GEN_FAKE").map(|v| !v.is_empty()).unwrap_or(false) {
        let prompt = match arg(args, "prompt-file") {
            Some(pf) => match read_prompt_file(io, &pf) {
                Ok(p) => Some(p),
                Err(c) => return c,
            },
            None => arg(args, "prompt"),
        };
        let out = arg(args, "out");
        let (Some(prompt), Some(out)) = (prompt.filter(|p| !p.is_empty()), out) else {
            io.err("generate-image: --prompt (or --prompt-file) and --out are required.\n");
            return 1;
        };
        let (w, h) = parse_size(&arg(args, "size").unwrap_or_else(|| "1536x1024".into()));
        let bytes = if out.ends_with(".svg") {
            svg_fake(&prompt, w as f64, h as f64).into_bytes()
        } else if transparent {
            png_fake_background(&prompt, w, h, true)
        } else {
            png_fake(&prompt, w, h)
        };
        if let Err(e) = std::fs::write(abs(&out), bytes) {
            io.err(&format!("Error: {}\n", node_read_error(&out, &e)));
            return 1;
        }
        io.out(&format!("IMAGE: {} ({}x{}, fake synthetic comp, $0.00, no API call)\n", out, w, h));
        note_generated_slot(io, &out);
        return 0;
    }
    let (backend, key) = match resolve_backend(&env) {
        Ok(Some(found)) => found,
        Ok(None) => {
            io.err("generate-image: no image API key is set (OPENAI_API_KEY or GEMINI_API_KEY); use the harness-native image tool instead.\n");
            return 1;
        }
        Err(reason) => {
            io.err(&format!("generate-image: {reason}.\n"));
            return 1;
        }
    };
    // Gemini image models have no alpha output. Keying out a painted ground
    // leaves halos on exactly the edges a cutout exists for, so refuse before
    // spending a request rather than ship a fake transparency.
    if backend == Backend::Gemini && transparent {
        io.err("generate-image: Gemini image models return opaque pixels, so --background transparent cannot be honored; report the transparency blocker, or generate an opaque plate with --background opaque.\n");
        return 1;
    }
    let prompt = match arg(args, "prompt-file") {
        Some(pf) => match read_prompt_file(io, &pf) {
            Ok(p) => Some(p),
            Err(c) => return c,
        },
        None => arg(args, "prompt"),
    };
    let out = arg(args, "out");
    let (Some(prompt), Some(out)) = (prompt.filter(|p| !p.is_empty()), out) else {
        io.err("generate-image: --prompt (or --prompt-file) and --out are required.\n");
        return 1;
    };
    let size = arg(args, "size").unwrap_or_else(|| "1536x1024".into());
    let quality = arg(args, "quality").unwrap_or_else(|| "medium".into());
    let mut refs: Vec<String> = Vec::new();
    for i in 0..args.len() {
        if args[i] == "--ref" {
            if let Some(n) = args.get(i + 1) {
                if !n.is_empty() && !n.starts_with("--") {
                    refs.push(n.clone());
                }
            }
        }
    }
    if backend == Backend::Gemini {
        let model = arg(args, "model").unwrap_or_else(|| gemini_model(&env));
        let job = GeminiJob { prompt: &prompt, out: &out, size: &size, quality: &quality, model: &model, refs: &refs, background: background.as_deref() };
        return run_gemini(io, &key, gemini_base, &job);
    }
    let model = arg(args, "model").unwrap_or_else(|| DEFAULT_MODEL.into());
    let agent = crate::http::agent_builder(api_base).build();
    let response = if !refs.is_empty() {
        let boundary = format!("----impeccable{:x}", crate::util::now_ms() as u64);
        let mut body: Vec<u8> = Vec::new();
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(format!("--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n", boundary, name, value).as_bytes());
        };
        field("model", &model);
        field("prompt", &prompt);
        field("size", &size);
        field("quality", &quality);
        field("n", "1");
        if let Some(background) = &background {
            field("background", background);
            field("output_format", "png");
        }
        for r in &refs {
            let (bytes, ty, filename) = match load_ref(io, &cwd, r) {
                Ok(loaded) => loaded,
                Err(code) => return code,
            };
            body.extend_from_slice(
                format!("--{}\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n", boundary, filename, ty).as_bytes(),
            );
            body.extend_from_slice(&bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
        agent
            .post(&format!("{api_base}/images/edits"))
            .set("Authorization", &format!("Bearer {}", key))
            .set("Content-Type", &format!("multipart/form-data; boundary={}", boundary))
            .send_bytes(&body)
    } else {
        let mut m = Map::new();
        m.insert("model".into(), Value::String(model.clone()));
        m.insert("prompt".into(), Value::String(prompt.clone()));
        m.insert("size".into(), Value::String(size.clone()));
        m.insert("quality".into(), Value::String(quality.clone()));
        m.insert("n".into(), Value::from(1));
        if let Some(background) = &background {
            m.insert("background".into(), Value::String(background.clone()));
            m.insert("output_format".into(), Value::String("png".into()));
        }
        agent
            .post(&format!("{api_base}/images/generations"))
            .set("Authorization", &format!("Bearer {}", key))
            .set("content-type", "application/json")
            .send_string(&serde_json::to_string(&Value::Object(m)).unwrap())
    };
    let (status, text) = match response {
        Ok(r) => {
            let st = r.status();
            (st, r.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => {
            io.err(&format!("TypeError: fetch failed: {}\n", e));
            return 1;
        }
    };
    if !(200..300).contains(&status) {
        let snippet: String = text.chars().take(300).collect();
        io.err(&format!("generate-image: API error {}: {}\n", status, snippet));
        return 1;
    }
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let b64 = json.get("data").and_then(|d| d.get(0)).and_then(|d| d.get("b64_json")).and_then(|b| b.as_str()).filter(|s| !s.is_empty());
    let Some(b64) = b64 else {
        io.err("generate-image: no image in response\n");
        return 1;
    };
    let bytes = base64_decode(b64);
    let _ = std::fs::write(abs(&out), bytes);
    // best-effort embed + sidecar
    let embedded = embed(io, &out, &prompt);
    {
        let mut m = Map::new();
        m.insert("prompt".into(), Value::String(prompt.clone()));
        m.insert("createdAt".into(), Value::String(iso_now()));
        m.insert("tool".into(), Value::String("impeccable generate-image".into()));
        m.insert("model".into(), Value::String(model.clone()));
        if let Some(background) = &background {
            m.insert("background".into(), Value::String(background.clone()));
            m.insert("outputFormat".into(), Value::String("png".into()));
        }
        if !refs.is_empty() {
            m.insert("refs".into(), Value::Array(refs.iter().cloned().map(Value::String).collect()));
        }
        let _ = std::fs::write(abs(&format!("{}.json", out)), json_pretty(&Value::Object(m)));
    }
    note_generated_slot(io, &out);
    io.out(&format!(
        "IMAGE: {} ({}, {}, {}, billed to your OpenAI key); {} at {}.json\n",
        out,
        size,
        quality,
        model,
        if embedded { "prompt embedded + sidecar" } else { "sidecar" },
        out
    ));
    0
}

/// A written image that fills a decision page's declared slot counts as that
/// hand's comp even when its bytes match what sat there before (the same
/// prompt regenerates the same bytes). A failed record is reported, not fatal:
/// the image itself was written.
fn note_generated_slot(io: &mut Io, out: &str) {
    let cwd = io.cwd.to_string_lossy().into_owned();
    if let Err(e) = crate::serve_question::record_generated(&cwd, out) {
        io.err(&format!("generate-image: could not mark {} as generated for its decision page ({}); a --wait may report it stale\n", out, e));
    }
}

fn base64_decode(s: &str) -> Vec<u8> {
    let table = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in s.bytes() {
        let Some(v) = table(b) else { continue };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn round_trip(edit: bool, override_model: Option<&str>, background: Option<&str>) {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let api_base = format!("http://{}", server.server_addr());
        let temp = std::env::temp_dir().join(format!("impeccable-image-{}-{}", std::process::id(), server.server_addr().to_ip().unwrap().port()));
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("ref.png"), png_fake("reference", 16, 16)).unwrap();
        let handle = std::thread::spawn(move || {
            let mut request = server.recv_timeout(Duration::from_secs(10)).unwrap().expect("image request");
            let path = request.url().to_string();
            let content_type = request.headers().iter().find(|h| h.field.equiv("Content-Type")).unwrap().value.to_string();
            let mut body = String::new();
            // Multipart carries binary PNG bytes; preserve ASCII fields for inspection.
            let mut bytes = Vec::new();
            request.as_reader().read_to_end(&mut bytes).unwrap();
            body.push_str(&String::from_utf8_lossy(&bytes));
            request.respond(tiny_http::Response::from_string(r#"{"data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAQAAAABCAYAAAD5PA/NAAAAGklEQVR4nGP4////f7mAigYGBgaG/////wMAUdQJXhk2RAEAAAAASUVORK5CYII="}]}"#)).unwrap();
            (path, content_type, body)
        });
        let env = Env::from([("OPENAI_API_KEY".into(), "test-key".into())]);
        let (mut io, captured) = Io::captured("", temp.clone(), env);
        let mut args: Vec<String> = ["--prompt", "Comp regression", "--out", "comp.png", "--quality", "high"].iter().map(|s| s.to_string()).collect();
        if edit {
            args.extend(["--ref".into(), "ref.png".into()]);
        }
        if let Some(model) = override_model {
            args.extend(["--model".into(), model.into()]);
        }
        if let Some(background) = background {
            args.extend(["--background".into(), background.into()]);
        }
        let exit = run_with_api_base(&args, &mut io, &api_base);
        let (path, content_type, body) = handle.join().unwrap();
        let sidecar: Value = serde_json::from_slice(&std::fs::read(temp.join("comp.png.json")).unwrap()).unwrap();
        let image = std::fs::read(temp.join("comp.png")).unwrap();
        std::fs::remove_dir_all(&temp).unwrap();
        assert_eq!(exit, 0);
        let model = override_model.unwrap_or("gpt-image-2.5-flare");
        if edit {
            assert_eq!(path, "/images/edits");
            assert!(content_type.starts_with("multipart/form-data; boundary="));
            assert!(body.contains(&format!("name=\"model\"\r\n\r\n{model}\r\n")));
            assert!(body.contains("name=\"image[]\"; filename=\"ref.png\""));
            assert_eq!(sidecar["refs"], serde_json::json!(["ref.png"]));
            if let Some(background) = background {
                assert!(body.contains(&format!("name=\"background\"\r\n\r\n{background}\r\n")));
                assert!(body.contains("name=\"output_format\"\r\n\r\npng\r\n"));
            } else {
                assert!(!body.contains("name=\"background\""));
            }
        } else {
            assert_eq!(path, "/images/generations");
            assert_eq!(content_type, "application/json");
            let body: Value = serde_json::from_str(&body).unwrap();
            let mut expected = serde_json::json!({"model": model, "prompt": "Comp regression", "size": "1536x1024", "quality": "high", "n": 1});
            if let Some(background) = background {
                expected["background"] = background.into();
                expected["output_format"] = "png".into();
            }
            assert_eq!(body, expected);
        }
        if let Some(background) = background {
            assert_eq!(sidecar["background"], background);
            assert_eq!(sidecar["outputFormat"], "png");
        } else {
            assert!(sidecar.get("background").is_none());
        }
        // The server's PNG contains clear, partial, near-opaque and opaque pixels.
        // Embedding may add metadata before IEND, but must preserve all image chunks.
        let original = base64_decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAABCAYAAAD5PA/NAAAAGklEQVR4nGP4////f7mAigYGBgaG/////wMAUdQJXhk2RAEAAAAASUVORK5CYII=");
        assert!(image.starts_with(&original[..original.len() - 12]));
        assert_eq!(sidecar["model"], model);
        assert_eq!(sidecar["prompt"], "Comp regression");
        assert!(image.starts_with(b"\x89PNG\r\n\x1a\n"));
        let stdout = String::from_utf8(captured.stdout.borrow().clone()).unwrap();
        assert!(stdout.contains(&format!("{model}, billed to your OpenAI key")));
        assert!(stdout.contains("prompt embedded + sidecar"));
    }

    /// Serves `GET` requests from `routes` (path -> status, content type,
    /// body) and answers one `POST` with a generated image; returns every
    /// request it saw as (method, path, body).
    fn serve(server: tiny_http::Server, routes: Vec<(&'static str, u16, &'static str, Vec<u8>)>, requests: usize) -> std::thread::JoinHandle<Vec<(String, String, String)>> {
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            while seen.len() < requests {
                let Some(mut request) = server.recv_timeout(Duration::from_secs(10)).unwrap() else { break };
                let mut bytes = Vec::new();
                request.as_reader().read_to_end(&mut bytes).unwrap();
                let (method, path) = (request.method().to_string(), request.url().to_string());
                seen.push((method.clone(), path.clone(), String::from_utf8_lossy(&bytes).into_owned()));
                if method == "POST" {
                    request.respond(tiny_http::Response::from_string(r#"{"data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAQAAAABCAYAAAD5PA/NAAAAGklEQVR4nGP4////f7mAigYGBgaG/////wMAUdQJXhk2RAEAAAAASUVORK5CYII="}]}"#)).unwrap();
                } else if let Some((_, status, ty, body)) = routes.iter().find(|r| r.0 == path) {
                    let mut response = tiny_http::Response::from_data(body.clone()).with_status_code(*status);
                    if !ty.is_empty() {
                        response = response.with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], ty.as_bytes()).unwrap());
                    }
                    request.respond(response).unwrap();
                } else {
                    request.respond(tiny_http::Response::from_string("not found").with_status_code(404)).unwrap();
                }
            }
            seen
        })
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("impeccable-image-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn url_refs_are_downloaded_attached_and_recorded_as_urls() {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr());
        let dir = temp("url-ref");
        std::fs::write(dir.join("local.png"), png_fake("local", 8, 8)).unwrap();
        let webp = [&b"RIFF\x10\0\0\0WEBPVP8 "[..], &[0u8; 8][..]].concat();
        let handle = serve(
            server,
            vec![
                // The bytes decide the type, whatever the header and the
                // URL's extension say (or do not say).
                ("/cards/world.webp?v=2", 200, "image/webp; charset=binary", webp),
                ("/cards/world-hero.png", 200, "application/octet-stream", png_fake("hero", 8, 8)),
                ("/cards/blob", 200, "", vec![0xff, 0xd8, 0xff, 0xe0, 1, 2, 3]),
                ("/cards/mislabeled.webp", 200, "image/webp", png_fake("mislabeled", 8, 8)),
            ],
            5,
        );
        let (board, hero, blob, mislabeled) = (format!("{base}/cards/world.webp?v=2"), format!("{base}/cards/world-hero.png"), format!("{base}/cards/blob"), format!("{base}/cards/mislabeled.webp"));
        let env = Env::from([("OPENAI_API_KEY".into(), "test-key".into())]);
        let (mut io, captured) = Io::captured("", dir.clone(), env);
        let args: Vec<String> = ["--prompt", "Comp with cards", "--out", "comp.png", "--ref", &board, "--ref", "local.png", "--ref", &hero, "--ref", &blob, "--ref", &mislabeled].iter().map(|s| s.to_string()).collect();
        let exit = run_with_api_base(&args, &mut io, &base);
        let seen = handle.join().unwrap();
        let stderr = String::from_utf8(captured.stderr.borrow().clone()).unwrap();
        assert_eq!(exit, 0, "{stderr}");
        let paths: Vec<(&str, &str)> = seen.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
        assert_eq!(paths, vec![("GET", "/cards/world.webp?v=2"), ("GET", "/cards/world-hero.png"), ("GET", "/cards/blob"), ("GET", "/cards/mislabeled.webp"), ("POST", "/images/edits")]);
        let body = &seen[4].2;
        assert!(body.contains("name=\"image[]\"; filename=\"world.webp\"\r\nContent-Type: image/webp\r\n\r\nRIFF"), "{body}");
        assert!(body.contains("name=\"image[]\"; filename=\"local.png\"\r\nContent-Type: image/png\r\n"), "{body}");
        assert!(body.contains("name=\"image[]\"; filename=\"world-hero.png\"\r\nContent-Type: image/png\r\n"), "{body}");
        assert!(body.contains("name=\"image[]\"; filename=\"blob\"\r\nContent-Type: image/jpeg\r\n"), "{body}");
        assert!(body.contains("name=\"image[]\"; filename=\"mislabeled.webp\"\r\nContent-Type: image/png\r\n"), "{body}");
        // The sidecar records each ref as it was given: the URL itself.
        let sidecar: Value = serde_json::from_slice(&std::fs::read(dir.join("comp.png.json")).unwrap()).unwrap();
        assert_eq!(sidecar["refs"], serde_json::json!([board, "local.png", hero, blob, mislabeled]));
        assert!(dir.join("comp.png").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_url_ref_names_the_url_and_writes_nothing() {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr());
        let dir = temp("url-ref-fail");
        let handle = serve(server, vec![("/cards/page.html", 200, "text/html", b"<html>blocked</html>".to_vec()), ("/cards/world.webp", 200, "image/webp", b"<html>sign in</html>".to_vec())], 3);
        let run_ref = |url: &str| {
            let env = Env::from([("OPENAI_API_KEY".into(), "test-key".into())]);
            let (mut io, captured) = Io::captured("", dir.clone(), env);
            let args: Vec<String> = ["--prompt", "p", "--out", "comp.png", "--ref", url].iter().map(|s| s.to_string()).collect();
            let exit = run_with_api_base(&args, &mut io, &base);
            let err = String::from_utf8(captured.stderr.borrow().clone()).unwrap();
            let out = String::from_utf8(captured.stdout.borrow().clone()).unwrap();
            (exit, err, out)
        };
        // A missing image, then a page that is not an image.
        let missing = format!("{base}/cards/gone.webp");
        let (exit, err, out) = run_ref(&missing);
        assert_eq!((exit, out.as_str()), (1, ""));
        assert_eq!(err, format!("generate-image: could not download --ref {missing}: HTTP 404; nothing was generated.\n"));
        let page = format!("{base}/cards/page.html");
        let (exit, err, _) = run_ref(&page);
        assert_eq!(exit, 1);
        assert_eq!(err, format!("generate-image: could not download --ref {page}: the response is not a png, webp, or jpeg image; nothing was generated.\n"));
        // A 200 page behind an image URL and an image Content-Type is still not an image.
        let wall = format!("{base}/cards/world.webp");
        let (exit, err, _) = run_ref(&wall);
        assert_eq!(exit, 1);
        assert_eq!(err, format!("generate-image: could not download --ref {wall}: the response is not a png, webp, or jpeg image; nothing was generated.\n"));
        // Only the three downloads reached the server: no generation request.
        let seen = handle.join().unwrap();
        assert!(seen.iter().all(|r| r.0 == "GET"), "{seen:?}");
        // An unreachable host reads the same on every OS.
        let (exit, err, _) = run_ref("http://127.0.0.1:9/ref.png");
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: could not download --ref http://127.0.0.1:9/ref.png: the request failed; nothing was generated.\n");
        assert!(!dir.join("comp.png").exists() && !dir.join("comp.png.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fake_mode_never_requests_a_url_ref() {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/cards/world.webp", server.server_addr());
        let dir = temp("url-ref-fake");
        let env = Env::from([("IMPECCABLE_IMAGE_GEN_FAKE".into(), "1".into()), ("OPENAI_API_KEY".into(), "test-key".into())]);
        let (mut io, captured) = Io::captured("", dir.clone(), env);
        let args: Vec<String> = ["--prompt", "p", "--out", "comp.png", "--ref", &url].iter().map(|s| s.to_string()).collect();
        assert_eq!(run(&args, &mut io), 0);
        assert!(String::from_utf8(captured.stdout.borrow().clone()).unwrap().contains("fake synthetic comp"));
        assert!(server.recv_timeout(Duration::from_millis(300)).unwrap().is_none());
        assert!(dir.join("comp.png").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn url_file_names_and_types() {
        assert_eq!(url_file_name("https://x.test/cards/a-hero.webp?v=1#f"), "a-hero.webp");
        assert_eq!(url_file_name("https://x.test/"), "ref");
        assert_eq!(url_file_name("https://x.test"), "ref");
        assert_eq!(sniffed_type(b"<html>"), None);
        assert_eq!(sniffed_type(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        assert!(is_url("http://a") && is_url("https://a") && !is_url("refs/a.png") && !is_url("ftp://a"));
    }

    #[test]
    fn generation_uses_image_25_and_records_model() {
        round_trip(false, None, None);
    }

    #[test]
    fn reference_edit_uses_image_25_and_records_model() {
        round_trip(true, None, None);
    }

    #[test]
    fn generation_accepts_model_override() {
        round_trip(false, Some("gpt-image-2"), None);
    }

    #[test]
    fn reference_edit_accepts_sunburst_override() {
        round_trip(true, Some("gpt-image-2.5-sunburst"), None);
    }

    #[test]
    fn transparent_generation_preserves_alpha_and_provenance() {
        round_trip(false, None, Some("transparent"));
    }

    #[test]
    fn transparent_edit_preserves_alpha_and_provenance() {
        round_trip(true, Some("gpt-image-2.5-sunburst"), Some("transparent"));
    }

    #[test]
    fn opaque_background_is_explicit() {
        round_trip(false, None, Some("opaque"));
    }

    #[test]
    fn fake_cutout_has_real_alpha_and_default_fake_stays_rgb() {
        use std::io::Read;
        let png = png_fake_background("cutout", 16, 16, true);
        assert_eq!(png[25], 6); // RGBA
        assert_eq!(png_fake("comp", 16, 16)[25], 2); // RGB, legacy fake output
        let mut offset = 8;
        let mut raw = Vec::new();
        while offset + 12 <= png.len() {
            let size = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
            if &png[offset + 4..offset + 8] == b"IDAT" {
                flate2::read::ZlibDecoder::new(&png[offset + 8..offset + 8 + size])
                    .read_to_end(&mut raw)
                    .unwrap();
            }
            offset += size + 12;
        }
        assert_eq!(raw[4], 0); // transparent corner
        assert_eq!(raw[8 * (16 * 4 + 1) + 1 + 8 * 4 + 3], 255); // opaque subject
    }

    #[test]
    fn api_base_reads_openai_base_url() {
        let env = |v: &str| Env::from([("OPENAI_BASE_URL".into(), v.into())]);
        assert_eq!(api_base(&Env::new()), "https://api.openai.com/v1");
        assert_eq!(api_base(&env("")), "https://api.openai.com/v1");
        assert_eq!(api_base(&env("  ")), "https://api.openai.com/v1");
        assert_eq!(api_base(&env("http://127.0.0.1:8790/openai/v1/")), "http://127.0.0.1:8790/openai/v1");
        assert_eq!(api_base(&env("https://proxy.example/v1")), "https://proxy.example/v1");
    }

    fn env_of(pairs: &[(&str, &str)]) -> Env {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A `generateContent` answer whose parts are `parts`, ending in STOP.
    fn gemini_answer(parts: Value) -> String {
        serde_json::json!({ "candidates": [{ "content": { "role": "model", "parts": parts }, "finishReason": "STOP" }] }).to_string()
    }

    /// Answers one request with `status` and `body`; returns what it saw as
    /// (path, x-goog-api-key, body).
    fn serve_once(server: tiny_http::Server, status: u16, body: String) -> std::thread::JoinHandle<(String, String, Vec<u8>)> {
        std::thread::spawn(move || {
            let mut request = server.recv_timeout(Duration::from_secs(10)).unwrap().expect("gemini request");
            let path = request.url().to_string();
            let key = request.headers().iter().find(|h| h.field.equiv("x-goog-api-key")).map(|h| h.value.to_string()).unwrap_or_default();
            let mut bytes = Vec::new();
            request.as_reader().read_to_end(&mut bytes).unwrap();
            let response = tiny_http::Response::from_string(body)
                .with_status_code(status)
                .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
            request.respond(response).unwrap();
            (path, key, bytes)
        })
    }

    fn run_gemini_args(dir: &std::path::Path, env: Env, base: &str, args: &[&str]) -> (i32, String, String) {
        let (mut io, captured) = Io::captured("", dir.to_path_buf(), env);
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let exit = run_with_bases(&args, &mut io, "http://127.0.0.1:9/openai-must-not-be-called", base);
        let out = String::from_utf8(captured.stdout.borrow().clone()).unwrap();
        let err = String::from_utf8(captured.stderr.borrow().clone()).unwrap();
        (exit, out, err)
    }

    #[test]
    fn backend_resolution_keeps_openai_first_and_honors_a_pin() {
        let pick = |pairs: &[(&str, &str)]| resolve_backend(&env_of(pairs)).map(|o| o.map(|(b, k)| (b, k)));
        assert_eq!(pick(&[]), Ok(None));
        assert_eq!(pick(&[("OPENAI_API_KEY", "  ")]), Ok(None));
        assert_eq!(pick(&[("OPENAI_API_KEY", "sk")]), Ok(Some((Backend::OpenAi, "sk".into()))));
        assert_eq!(pick(&[("GEMINI_API_KEY", "g")]), Ok(Some((Backend::Gemini, "g".into()))));
        // Both keys: OpenAI, as before this backend existed, unless pinned.
        assert_eq!(pick(&[("OPENAI_API_KEY", "sk"), ("GEMINI_API_KEY", "g")]), Ok(Some((Backend::OpenAi, "sk".into()))));
        assert_eq!(pick(&[("OPENAI_API_KEY", "sk"), ("GEMINI_API_KEY", "g"), ("IMPECCABLE_IMAGE_PROVIDER", " Gemini ")]), Ok(Some((Backend::Gemini, "g".into()))));
        assert_eq!(pick(&[("GEMINI_API_KEY", "g"), ("IMPECCABLE_IMAGE_PROVIDER", "openai")]), Err("IMPECCABLE_IMAGE_PROVIDER is openai but OPENAI_API_KEY is not set".into()));
        assert_eq!(pick(&[("OPENAI_API_KEY", "sk"), ("IMPECCABLE_IMAGE_PROVIDER", "gemini")]), Err("IMPECCABLE_IMAGE_PROVIDER is gemini but GEMINI_API_KEY is not set".into()));
        assert_eq!(pick(&[("GEMINI_API_KEY", "g"), ("IMPECCABLE_IMAGE_PROVIDER", "imagen")]), Err("IMPECCABLE_IMAGE_PROVIDER must be openai or gemini, not \"imagen\"".into()));
        assert_eq!(gemini_model(&Env::new()), GEMINI_DEFAULT_MODEL);
        assert_eq!(gemini_model(&env_of(&[("IMPECCABLE_GEMINI_IMAGE_MODEL", " gemini-3.1-flash-image ")])), "gemini-3.1-flash-image");
    }

    #[test]
    fn no_key_names_both_keys() {
        let dir = temp("gemini-no-key");
        let (exit, out, err) = run_gemini_args(&dir, Env::new(), "http://127.0.0.1:9", &["--prompt", "p", "--out", "x.png"]);
        assert_eq!((exit, out.as_str()), (1, ""));
        assert_eq!(err, "generate-image: no image API key is set (OPENAI_API_KEY or GEMINI_API_KEY); use the harness-native image tool instead.\n");
        let (exit, _, err) = run_gemini_args(&dir, env_of(&[("GEMINI_API_KEY", "g"), ("IMPECCABLE_IMAGE_PROVIDER", "dall-e")]), "http://127.0.0.1:9", &["--prompt", "p", "--out", "x.png"]);
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: IMPECCABLE_IMAGE_PROVIDER must be openai or gemini, not \"dall-e\".\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn aspect_and_tier_follow_the_requested_size() {
        assert_eq!(nearest_aspect(1536, 1024), "3:2");
        assert_eq!(nearest_aspect(1024, 1536), "2:3");
        assert_eq!(nearest_aspect(1024, 1024), "1:1");
        assert_eq!(nearest_aspect(1920, 1080), "16:9");
        assert_eq!(nearest_aspect(1440, 900), "3:2");
        assert_eq!(nearest_aspect(390, 844), "9:16");
        assert_eq!(nearest_aspect(2520, 1080), "21:9");
        assert_eq!(nearest_aspect(5000, 100), "21:9");
        assert_eq!(nearest_aspect(0, 0), "1:1");
        assert_eq!(gemini_image_size("gemini-3-pro-image", 1536, 1024, "medium"), Some("2K"));
        assert_eq!(gemini_image_size("gemini-3-pro-image", 800, 600, "medium"), Some("1K"));
        assert_eq!(gemini_image_size("gemini-3-pro-image", 800, 600, "high"), Some("2K"));
        assert_eq!(gemini_image_size("gemini-3.1-flash-image", 3000, 2000, "medium"), Some("4K"));
        assert_eq!(gemini_image_size("gemini-2.5-flash-image", 3000, 2000, "high"), None);
    }

    #[test]
    fn base64_encode_round_trips() {
        for len in 0..10 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 250) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)), bytes);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn gemini_generation_sends_refs_inline_and_keeps_the_final_image() {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1beta", server.server_addr());
        let dir = temp("gemini-round-trip");
        let reference = png_fake("reference", 8, 8);
        std::fs::write(dir.join("ref.png"), &reference).unwrap();
        let (thought, last) = (png_fake("interim thought", 4, 4), png_fake("final comp", 6, 4));
        let answer = gemini_answer(serde_json::json!([
            { "text": "Planning the layout.", "thought": true },
            { "inlineData": { "mimeType": "image/png", "data": base64_encode(&thought) }, "thought": true },
            { "text": "Here is the comp." },
            { "inlineData": { "mimeType": "image/png", "data": base64_encode(&last) } },
        ]));
        let handle = serve_once(server, 200, answer);
        let env = env_of(&[("GEMINI_API_KEY", "gm-key")]);
        let (exit, out, err) = run_gemini_args(&dir, env, &base, &["--prompt", "A booking dashboard comp.", "--out", "comp.png", "--ref", "ref.png", "--quality", "high"]);
        let (path, key, body) = handle.join().unwrap();
        assert_eq!(exit, 0, "{err}");
        assert_eq!(path, "/v1beta/models/gemini-3-pro-image:generateContent");
        assert_eq!(key, "gm-key");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "contents": [{ "role": "user", "parts": [
                    { "inlineData": { "mimeType": "image/png", "data": base64_encode(&reference) } },
                    { "text": "A booking dashboard comp." },
                ]}],
                "generationConfig": { "responseModalities": ["TEXT", "IMAGE"], "imageConfig": { "aspectRatio": "3:2", "imageSize": "2K" } },
            })
        );
        // The final image, not the thought, with the prompt embedded before IEND.
        let image = std::fs::read(dir.join("comp.png")).unwrap();
        assert!(image.starts_with(&last[..last.len() - 12]));
        let sidecar: Value = serde_json::from_slice(&std::fs::read(dir.join("comp.png.json")).unwrap()).unwrap();
        assert_eq!(sidecar["provider"], "gemini");
        assert_eq!(sidecar["model"], "gemini-3-pro-image");
        assert_eq!(sidecar["aspectRatio"], "3:2");
        assert_eq!(sidecar["imageSize"], "2K");
        assert_eq!(sidecar["refs"], serde_json::json!(["ref.png"]));
        assert_eq!(sidecar["prompt"], "A booking dashboard comp.");
        assert_eq!(out, "IMAGE: comp.png (6x4, 3:2 at 2K, gemini-3-pro-image, billed to your Gemini API key); prompt embedded + sidecar at comp.png.json\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gemini_jpeg_answer_becomes_a_png_and_model_comes_from_env() {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr());
        let dir = temp("gemini-jpeg");
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(10, 6, image::Rgb([200, 80, 40])).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]));
        let handle = serve_once(server, 200, gemini_answer(serde_json::json!([{ "inline_data": { "mime_type": "image/jpeg", "data": base64_encode(&jpeg) } }])));
        let env = env_of(&[("GEMINI_API_KEY", "g"), ("IMPECCABLE_GEMINI_IMAGE_MODEL", "gemini-2.5-flash-image")]);
        let (exit, out, err) = run_gemini_args(&dir, env, &base, &["--prompt", "p", "--out", "plate.png", "--size", "1024x1536", "--background", "opaque"]);
        let (path, _, body) = handle.join().unwrap();
        assert_eq!(exit, 0, "{err}");
        assert_eq!(path, "/models/gemini-2.5-flash-image:generateContent");
        // Gemini 2.x takes no imageSize.
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["generationConfig"]["imageConfig"], serde_json::json!({ "aspectRatio": "2:3" }));
        let png = std::fs::read(dir.join("plate.png")).unwrap();
        assert!(impeccable_comp::png_io::is_png(&png));
        assert_eq!(image_dims(&png), Some((10, 6)));
        let sidecar: Value = serde_json::from_slice(&std::fs::read(dir.join("plate.png.json")).unwrap()).unwrap();
        assert_eq!(sidecar["background"], "opaque");
        assert!(sidecar.get("imageSize").is_none());
        assert_eq!(out, "IMAGE: plate.png (10x6, 2:3, gemini-2.5-flash-image, billed to your Gemini API key); prompt embedded + sidecar at plate.png.json\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gemini_refuses_what_it_cannot_honor_before_any_request() {
        // Nothing listens at the base: any request would fail differently.
        let dir = temp("gemini-refusals");
        let env = || env_of(&[("GEMINI_API_KEY", "g")]);
        let (exit, out, err) = run_gemini_args(&dir, env(), "http://127.0.0.1:9", &["--prompt", "p", "--out", "cutout.png", "--background", "transparent"]);
        assert_eq!((exit, out.as_str()), (1, ""));
        assert_eq!(err, "generate-image: Gemini image models return opaque pixels, so --background transparent cannot be honored; report the transparency blocker, or generate an opaque plate with --background opaque.\n");
        let (exit, _, err) = run_gemini_args(&dir, env(), "http://127.0.0.1:9", &["--prompt", "p", "--out", "comp.svg"]);
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: the Gemini backend writes raster images; use a .png --out.\n");
        let (exit, _, err) = run_gemini_args(&dir, env(), "http://127.0.0.1:9", &["--prompt", "p", "--out", "comp.png", "--model", "../../files"]);
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: --model \"../../files\" is not a Gemini model name.\n");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gemini_failures_say_why_and_write_nothing() {
        let _proxy_lock = crate::http::PROXY_ENV_LOCK.lock().unwrap();
        let dir = temp("gemini-failures");
        let attempt = |status: u16, body: String, args: &[&str]| {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let base = format!("http://{}", server.server_addr());
            let handle = serve_once(server, status, body);
            let result = run_gemini_args(&dir, env_of(&[("GEMINI_API_KEY", "g")]), &base, args);
            handle.join().unwrap();
            result
        };
        let refusal = serde_json::json!({ "candidates": [{ "content": { "parts": [{ "text": "I can't draw that,\n  but here is a description." }] }, "finishReason": "IMAGE_SAFETY" }] }).to_string();
        let (exit, _, err) = attempt(200, refusal, &["--prompt", "p", "--out", "comp.png"]);
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: no image in response: finish reason IMAGE_SAFETY; the model answered in text: I can't draw that, but here is a description.\n");
        let blocked = serde_json::json!({ "promptFeedback": { "blockReason": "PROHIBITED_CONTENT" } }).to_string();
        let (_, _, err) = attempt(200, blocked, &["--prompt", "p", "--out", "comp.png"]);
        assert_eq!(err, "generate-image: no image in response: the prompt was blocked (PROHIBITED_CONTENT)\n");
        let (exit, _, err) = attempt(404, r#"{"error":{"code":404}}"#.into(), &["--prompt", "p", "--out", "comp.png", "--model", "models/gemini-9-image"]);
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: API error 404: {\"error\":{\"code\":404}} (no model gemini-9-image for this key: set IMPECCABLE_GEMINI_IMAGE_MODEL or pass --model with an image model the key can use)\n");
        let (_, _, err) = attempt(429, "{}".into(), &["--prompt", "p", "--out", "comp.png"]);
        assert!(err.ends_with("(quota exhausted: image models may need a billing-enabled project, which Google AI plan credits can fund)\n"), "{err}");
        // A PNG answer cannot be written as a JPEG out.
        let png = gemini_answer(serde_json::json!([{ "inlineData": { "mimeType": "image/png", "data": base64_encode(&png_fake("x", 4, 4)) } }]));
        let (exit, _, err) = attempt(200, png, &["--prompt", "p", "--out", "comp.jpg"]);
        assert_eq!(exit, 1);
        assert_eq!(err, "generate-image: Gemini returned image/png, not what --out names; use a .png --out, which converts.\n");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_background_requests_fail_before_network_or_output() {
        for flags in [
            vec!["--background"],
            vec!["--background", "white"],
            vec!["--background", "transparent", "--out", "cutout.jpg"],
            vec!["--background", "opaque", "--out", "hero.webp"],
            vec!["--background", "auto", "--out", "hero.svg"],
        ] {
            let (mut io, captured) = Io::captured("", std::env::temp_dir(), Env::new());
            let args = flags.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            assert_eq!(run(&args, &mut io), 1);
            let stderr = String::from_utf8(captured.stderr.borrow().clone()).unwrap();
            assert!(stderr.contains("--background"), "{stderr}");
            assert!(!stderr.contains("OPENAI_API_KEY"));
        }
    }
}
