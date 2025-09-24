use std::{env, fmt};
use std::collections::{HashMap};
use std::error::{Error};
use std::ffi::{OsStr, OsString};
use std::fs::{File};
use std::io::{Read, Write};
use std::path::{Path};

use html_escape::{encode_text as escape};
use tiny_http::{Method, Request, Response, Header};
use url::{Url};

/// Given `"foo.BAR"` and `"bar"` returns `Some("foo")`.
fn remove_extension<'f>(filename: &'f str, extension: &str) -> Option<&'f str> {
    if let Some(index) = filename.len().checked_sub(".".len() + extension.len()) {
        if let Some((ret, tail)) = filename.split_at_checked(index) {
            let mut tail = tail.chars();
            if let Some('.') = tail.next() {
                if extension.eq_ignore_ascii_case(tail.as_str()) { return Some(ret); }
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------

/// `Error` returned by `validate_name()` if it doesn't like the filename.
#[derive(Debug)]
pub struct DubiousFilename(OsString);

impl fmt::Display for DubiousFilename {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Filename {:?} contains unusual characters; only letters, digits and \"_-.\" are allowed", self.0)
    }
}

impl Error for DubiousFilename {}

/// If `s` only contains alphanumeric characters and characters in `_-.`,
/// returns it unchanged.
fn validate_name(s: &OsStr) -> Result<&str, DubiousFilename> {
    for b in s.as_encoded_bytes() {
        match b {
            b'0' .. b'9' => {},
            b'A' .. b'Z' => {},
            b'a' .. b'z' => {},
            b'_' | b'.' | b'-' => {}
            _ => { return Err(DubiousFilename(s.to_owned())); }
        }
    }
    Ok(s.to_str().unwrap())
}

// ----------------------------------------------------------------------------

/// A "200 OK" HTTP response.
#[derive(Debug)]
pub enum HttpOkay {
    File(File),
    Html(String),
    Jpeg(Vec<u8>),
}

// An erroneous HTTP response.
#[derive(Debug)]
pub enum HttpError {
    Invalid,
    NotFound,
    Error(Box<dyn Error>),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for HttpError {}

macro_rules! impl_from_for_error {
    ($e:ty) => {
        impl From<$e> for HttpError {
            fn from(e: $e) -> Self { HttpError::Error(e.into()) }
        }
    };
}

impl_from_for_error!(std::io::Error);
impl_from_for_error!(std::num::ParseIntError);
impl_from_for_error!(std::char::ParseCharError);
impl_from_for_error!(url::ParseError);
impl_from_for_error!(image::ImageError);
impl_from_for_error!(DubiousFilename);

// ----------------------------------------------------------------------------

/// Requested size of an image.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct Dimensions {
    /// The user-requested width.
    pub w: u32,

    /// The user-requested height.
    pub h: u32,
}

impl fmt::Display for Dimensions {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "?w={}&h={}", self.w, self.h)
    }
}

// ----------------------------------------------------------------------------

/// Information about a request.
#[derive(Default, Debug, Clone, Hash, PartialEq, Eq)]
struct Params {
    /// The user-requested width, if any.
    pub w: Option<u32>,

    /// The user-requested height, if any.
    pub h: Option<u32>,
}

impl Params {
    /// Fill in missing parameters with default values, and apply maxima.
    pub fn get_dimensions(&self) -> Dimensions {
        Dimensions {
            w: 2048 .min(if let Some(w) = self.w { w } else { 800 }),
            h: 2048 .min(if let Some(h) = self.h { h } else { 600 }),
        }
    }
}

/// Parse a u32, ignoring white-space, and mapping errors to `None`.
fn parse_u32(s: impl AsRef<str>) -> Option<u32> { s.as_ref().trim().parse::<u32>().ok() }

impl FromIterator<(String, String)> for Params {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        let mut ret = Self::default();
        for (key, value) in iter.into_iter() {
            if "w" == key { ret.w = parse_u32(value); }
            else if "h" == key { ret.h = parse_u32(value); }
        }
        ret
    }
}

// ----------------------------------------------------------------------------

/// Contents of an album directory.
#[derive(Default, Debug, Clone)]
struct Album {
    readme: Option<String>,
    jpegs: Vec<String>,
    others: Vec<String>,
}

impl Album {
    fn new(dir_name: &Path) -> Result<Self, HttpError> {
        let mut ret = Self::default();
        for dir_entry in dir_name.read_dir()? {
            if let Some(filename) = dir_entry?.path().file_name() {
                let filename = validate_name(filename)?;
                if filename == "README.txt" {
                    ret.readme = Some(filename.into());
                } else {
                    if let Some(_) = remove_extension(filename, "jpg") {
                        ret.jpegs.push(filename.into());
                    } else {
                        ret.others.push(filename.into());
                    }
                }
            }
        }
        ret.jpegs.sort();
        ret.others.sort();
        Ok(ret)
    }
}

// ----------------------------------------------------------------------------

struct PhotoServer<'a> {
    /// Web server.
    pub server: tiny_http::Server,

    /// The external URL of the server.
    pub base_url: Url,

    /// The directory containing the photos.
    pub document_root: &'a Path,

    /// The thumbnail cache directory.
    pub thumbnail_root: &'a Path,
}

impl<'a> PhotoServer<'a> {
    fn new(addr: &str, base_url: &str, document_root: &'a str, thumbnail_root: &'a str) -> Self {
        let server = Self {
            server: tiny_http::Server::http(addr)
                .expect("Could not create the web server"),
            base_url: url::Url::parse(base_url)
                .expect("Could not parse the base URL"),
            document_root: Path::new(document_root),
            thumbnail_root: Path::new(thumbnail_root),
        };
        server
    }

    /// Load `jpeg_name`, resize it, and encode it as a new JPEG file.
    fn resize_jpeg(jpeg_name: &Path, d: Dimensions) -> Result<Vec<u8>, HttpError> {
        let image = image::open(jpeg_name)?;
        let image = image.resize(d.w, d.h, image::imageops::FilterType::Lanczos3);
        let mut ret = Vec::<u8>::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut ret, 85).encode_image(&image)?;
        Ok(ret)
    }

    /// Show thumbnails for all photos in a directory.
    pub fn index(&self, dir_name: &str, params: &Params) -> Result<HttpOkay, HttpError> {
        let dimensions = params.get_dimensions();
        let album = Album::new(&self.document_root.join(dir_name))?;
        let readme = if let Some(name) = &album.readme {
            let mut text = String::new();
            File::open(self.document_root.join(dir_name).join(name))?.read_to_string(&mut text)?;
            format!(
                "<pre>{text}</pre>",
                text = escape(&text),
            )
        } else {
            String::new()
        };
        let jpegs: Vec<_> = album.jpegs.iter().map(|name| format!(
            r#"<a href="{name}.html{dimensions}"><img src="{name}.thumb"/></a>"#,
            name = name,
        )).collect();
        let others: Vec<_> = album.others.iter().map(|name| format!(
            r#"<a href="{name}">{name}</a>"#,
            name = name,
        )).collect();
        Ok(HttpOkay::Html(format!(
r#"<html>
 <head>
  <title>{dir_name}</title>
 </head>
 <body>
  <h2>{dir_name}</h2>
  <a href="..">Up</a><br/>
  {readme}
  {jpegs}
  <br/>
  {others}
 </body>
</html>"#,
            dir_name = dir_name,
            readme = readme,
            jpegs = jpegs.join("\n  "),
            others = others.join("\n  "),
        )))
    }

    /// Serve a resized JPEG file.
    pub fn rescale(&self, dir_name: &str, leaf_name: &str, params: &Params) -> Result<HttpOkay, HttpError> {
        let jpeg_name = self.document_root.join(dir_name).join(leaf_name);
        Ok(HttpOkay::Jpeg(Self::resize_jpeg(&jpeg_name, params.get_dimensions())?))
    }

    /// Show an HTML frame around a single photo.
    pub fn frame(&self, dir_name: &str, leaf_name: &str, params: &Params) -> Result<HttpOkay, HttpError> {
        let dimensions = params.get_dimensions();
        // Enumerate the JPEG files in `dir_name` and compute
        // `prev` and `next` links.
        let mut paths = Album::new(&self.document_root.join(dir_name))?.jpegs;
        paths.sort();
        let mut previouses: HashMap<&str, &str> = HashMap::new();
        let mut nexts: HashMap<&str, &str> = HashMap::new();
        let mut prev = paths.last().ok_or(HttpError::NotFound)?;
        for p in &paths {
            previouses.insert(&p, &prev);
            nexts.insert(&prev, &p);
            prev = p;
        }
        // This substring contains a lot of `{` and `}` characters.
        let stylesheet =
r#"body {background-color: #000000; color: #FFFFFF}
a:link {color: #8080FF}
a:visited {color: #C080FF}
input[type="text"] {
background-color: #404040; color: #FFFFFF;
border: thin solid #808080
}"#;
        // Generate HTML.
        Ok(HttpOkay::Html(format!(
r#"<html>
<head>
<title>{dir_name}/{base_name}</title>
<style type="text/css">
{stylesheet}
</style>
</head>
<body>
<center><h3>{dir_name}/{base_name}</h3></center>
<form action="{leaf_name}.html" method="get">
<table align="center" valign="center">
<tr>
<td colspan="3" align="center">
<a href="{previous}.html{dimensions}">previous</a>
<a href="{next}.html{dimensions}">next</a>
<a href=".{dimensions}">up</a>
<a href="{leaf_name}">original</a>
</td>
</tr>
<tr>
<td colspan="3" align="center">
<img src="{leaf_name}{dimensions}"/>
</td>
</tr>
<tr>
<td>Width <input type="text" name="w" value="{w}"/></td>
<td>Height <input type="text" name="h" value="{h}"/></td>
<td><input type="submit" value="Change size"/></td>
</tr>
</table>
</form>
</body>
</html>"#,
            dir_name = dir_name,
            base_name = remove_extension(leaf_name, "jpg").unwrap(), // Checked by caller.
            leaf_name = leaf_name,
            previous = previouses.get(leaf_name).ok_or(HttpError::NotFound)?,
            next = nexts.get(leaf_name).ok_or(HttpError::NotFound)?,
            dimensions = dimensions,
            w = dimensions.w,
            h = dimensions.h,
        )))
    }

    /// Serve a JPEG thumbnail.
    pub fn thumb(&self, dir_name: &str, leaf_name: &str, _params: &Params) -> Result<HttpOkay, HttpError> {
        let thumbnail_dir = self.thumbnail_root.join(dir_name);
        std::fs::create_dir_all(&thumbnail_dir)?;
        let thumbnail_name = thumbnail_dir.join(leaf_name);
        if let Ok(mut file) = File::create_new(&thumbnail_name) {
            // Cached thumbnail file is missing; generate it.
            let jpeg_name = self.document_root.join(dir_name).join(leaf_name);
            file.write(&Self::resize_jpeg(&jpeg_name, Dimensions {w: 128, h: 96})?)?;
        }
        Ok(HttpOkay::File(File::open(&thumbnail_name)?))
    }

    /// Handle a single request.
    fn handle_request(&self, request: &Request) -> Result<HttpOkay, HttpError> {
        match request.method() {
            Method::Get => {},
            _ => return Err(HttpError::Invalid),
        }

        let absolute_url = self.base_url.join(request.url())?;
        println!("{} {}", request.remote_addr().unwrap().ip(), absolute_url);
        // Parse the query parameters.
        let params: Params = absolute_url.query_pairs().map(
            |(key, value)| (
                url_escape::decode(key.as_ref()).into_owned(),
                url_escape::decode(value.as_ref()).into_owned(),
            )
        ).collect();
        // Parse the path segments.
        // TODO: Abstract as `enum Route`?
        let mut path: Vec<String> = absolute_url.path_segments().ok_or(HttpError::Invalid)?.map(
            |s| url_escape::decode(s).into_owned()
        ).collect();
        if let Some(last) = path.last() {
            if "" == last { path.pop(); }
        }
        // Dispatch to the appropriate method.
        let mut path_iter = path.into_iter();
        let dir_name = &path_iter.next().ok_or(HttpError::Invalid)?;
        if let Some(leaf_name) = &path_iter.next() {
            if let Some(_) = remove_extension(leaf_name, "jpg") {
                if params.w.is_some() || params.h.is_some() {
                    return self.rescale(dir_name, leaf_name, &params);
                }
            } else if let Some(jpeg_name) = remove_extension(leaf_name, "html") {
                if let Some(_) = remove_extension(jpeg_name, "jpg") {
                    return self.frame(dir_name, &jpeg_name, &params);
                }
            } else if let Some(jpeg_name) = remove_extension(leaf_name, "thumb") {
                if let Some(_) = remove_extension(jpeg_name, "jpg") {
                    return self.thumb(dir_name, &jpeg_name, &params);
                }
            }
            // Any other `leaf_name` is a static file.
            let document_name = self.document_root.join(dir_name).join(leaf_name);
            return Ok(HttpOkay::File(File::open(&document_name)?));
        } else {
            return self.index(dir_name, &params);
        }
    }

    /// Construct an HTTP header.
    fn header(key: &str, value: &str) -> tiny_http::Header {
        Header::from_bytes(
            key.as_bytes(),
            value.as_bytes(),
        ).unwrap() // depends only on data fixed at compile time
    }

    /// Handle requests for ever.
    fn handle_requests(&self) {
        for request in self.server.incoming_requests() {
            match self.handle_request(&request) {
                Ok(HttpOkay::File(file)) => {
                    request.respond(Response::from_file(file))
                },
                Ok(HttpOkay::Html(text)) => {
                    let header = Self::header("Content-Type", "text/html");
                    request.respond(Response::from_string(text).with_header(header))
                },
                Ok(HttpOkay::Jpeg(data)) => {
                    let header = Self::header("Content-Type", "image/jpeg");
                    request.respond(Response::from_data(data).with_header(header))
                },
                Err(HttpError::Invalid) => {
                    request.respond(Response::from_string("Invalid request").with_status_code(400))
                },
                Err(HttpError::NotFound) => {
                    request.respond(Response::from_string("Not found").with_status_code(404))
                },
                Err(e) => {
                    println!("Error: {}", e);
                    request.respond(Response::from_string("Internal error").with_status_code(500))
                },
            }.unwrap_or_else(|e2| println!("IO Error: {}", e2));
        }
    }
}

// ----------------------------------------------------------------------------

/// The default server address and port to listen on.
const SERVER_ADDRESS: &'static str = "127.0.0.1:8082";
const DOCUMENT_ROOT: &'static str = "./document_root";
const THUMBNAIL_ROOT: &'static str = "./thumbnail_root";

fn main() {
    let server_address = env::var("PHOTO_SERVER_ADDRESS").unwrap_or_else(|_| SERVER_ADDRESS.to_owned());
    let server_url = format!("http://{}", server_address);
    let base_url = env::var("PHOTO_SERVER_BASE_URL").unwrap_or_else(|_| server_url.clone());
    let document_root = env::var("PHOTO_SERVER_DOCUMENT_ROOT").unwrap_or_else(|_| DOCUMENT_ROOT.to_owned());
    let thumbnail_root = env::var("PHOTO_SERVER_THUMBNAIL_ROOT").unwrap_or_else(|_| THUMBNAIL_ROOT.to_owned());
    let server = PhotoServer::new(&server_address, &base_url, &document_root, &thumbnail_root);
    println!("Listening on {}", server_url);
    server.handle_requests();
}
