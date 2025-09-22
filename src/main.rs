use std::collections::{HashMap};
use std::error::{Error};
// use std::io::{Write};
use std::fs::{File};
use std::io::{Write};
use std::path::{Path, PathBuf};

use tiny_http::{Method, Request, Response, Header};
use url::{Url};

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

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
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

// ----------------------------------------------------------------------------

/// Requested size of an image.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct Dimensions {
    /// The user-requested width.
    pub w: u32,

    /// The user-requested height.
    pub h: u32,
}

impl std::fmt::Display for Dimensions {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "?w={}&h={}", self.w, self.h)
    }
}

// ----------------------------------------------------------------------------

/// Information about a request.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct Params {
    /// The user-requested width, if any.
    pub w: Option<u32>,

    /// The user-requested height, if any.
    pub h: Option<u32>,
}

impl Params {
    /// Parse query parameters.
    fn new(params: &HashMap<String, String>) -> Self {
        let parse_u32 = |key: &'static str| {
            params.get(key).and_then(|s| s.trim().parse::<u32>().ok())
        };
        Self {w: parse_u32("w"), h: parse_u32("h")}
    }

    /// Fill in missing parameters with default values, and apply maxima.
    pub fn get_dimensions(&self) -> Dimensions {
        Dimensions {
            w: 2048 .min(if let Some(w) = self.w { w } else { 800 }),
            h: 2048 .min(if let Some(h) = self.h { h } else { 600 }),
        }
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

    /// Construct an HTTP header.
    fn header(key: &str, value: &str) -> tiny_http::Header {
        let key_b = key.as_bytes();
        let val_b = value.as_bytes();
        Header::from_bytes(
            key_b, val_b)
            .unwrap() // depends only on data fixed at compile time
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

    /// Handle a single request.
    fn handle_request(&self, request: &Request) -> Result<HttpOkay, HttpError> {
        match request.method() {
            Method::Get => {},
            _ => return Err(HttpError::Invalid),
        }

        let url = request.url();
        let url = url_escape::decode(url).into_owned();
        let url = self.base_url.join(&url)?;
        println!("{} {}", request.remote_addr().unwrap().ip(), url);
        let params = Params::new(&url.query_pairs().map(
            |(key, value)| (key.into_owned(), value.into_owned())
        ).collect());
        let mut path = url.path_segments().unwrap();
        let dir_name = Path::new(path.next().ok_or(HttpError::Invalid)?);
        if let Some(leaf_name) = path.next() {
            let leaf_name = Path::new(leaf_name);
            if let Some(extension) = leaf_name.extension() {
                if extension == "jpg" || extension == "JPG" {
                    if params.w.is_none() && params.h.is_none() {
                        self.jpeg(dir_name, leaf_name, &params)
                    } else {
                        self.rescale(dir_name, leaf_name, &params)
                    }
                } else if extension == "html" {
                    self.frame(dir_name, &leaf_name.with_extension(""), &params)
                } else if extension == "thumb" {
                    self.thumb(dir_name, &leaf_name.with_extension(""), &params)
                } else {
                    println!("Invalid URL: {:?}", url);
                    Err(HttpError::Invalid)
                }
            } else {
                println!("Not found: {:?}", leaf_name);
                Err(HttpError::NotFound)
            }
        } else {
            self.index(dir_name, &params)
        }
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
    pub fn index(&self, _dir_name: &Path, _params: &Params) -> Result<HttpOkay, HttpError> {
        println!("index()");
        Err(HttpError::Invalid)
    }

    /// Serve a JPEG file directly.
    pub fn jpeg(&self, dir_name: &Path, leaf_name: &Path, _params: &Params) -> Result<HttpOkay, HttpError> {
        let jpeg_name = self.document_root.join(dir_name).join(leaf_name);
        Ok(HttpOkay::File(File::open(&jpeg_name)?))
    }

    /// Serve a resized JPEG file.
    pub fn rescale(&self, dir_name: &Path, leaf_name: &Path, params: &Params) -> Result<HttpOkay, HttpError> {
        let jpeg_name = self.document_root.join(dir_name).join(leaf_name);
        Ok(HttpOkay::Jpeg(Self::resize_jpeg(&jpeg_name, params.get_dimensions())?))
    }

    /// Show an HTML frame around a single photo.
    pub fn frame(&self, dir_name: &Path, leaf_name: &Path, params: &Params) -> Result<HttpOkay, HttpError> {
        let dimensions = params.get_dimensions();
        // Enumerate the files in `jpeg_dir` and compute `prev` and `next` links.
        let jpeg_dir = self.document_root.join(dir_name);
        let mut paths: Vec<PathBuf> = Vec::new();
        for dir_entry in jpeg_dir.read_dir()? {
            if let Some(path) = dir_entry?.path().file_name() {
                if let Some(extension) = Path::new(path).extension() {
                    if extension == "JPG" || extension == "jpg" { paths.push(path.into()); }
                }
            }
        }
        paths.sort();
        let mut previouses: HashMap<&Path, &Path> = HashMap::new();
        let mut nexts: HashMap<&Path, &Path> = HashMap::new();
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
            dir_name = dir_name.display(),
            base_name = leaf_name.with_extension("").display(),
            leaf_name = leaf_name.display(),
            previous = previouses.get(leaf_name).ok_or(HttpError::NotFound)?.display(),
            next = nexts.get(leaf_name).ok_or(HttpError::NotFound)?.display(),
            dimensions = dimensions,
            w = dimensions.w,
            h = dimensions.h,
        )))
    }

    /// Serve a JPEG thumbnail.
    pub fn thumb(&self, dir_name: &Path, leaf_name: &Path, _params: &Params) -> Result<HttpOkay, HttpError> {
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
}

// ----------------------------------------------------------------------------

/// The default server address and port to listen on.
const SERVER_ADDRESS: &'static str = "127.0.0.1:8082";
const DOCUMENT_ROOT: &'static str = "./document_root";
const THUMBNAIL_ROOT: &'static str = "./thumbnail_root";

fn main() {
    let server_address = std::env::var("PHOTO_SERVER_ADDRESS").unwrap_or_else(|_| SERVER_ADDRESS.to_owned());
    let server_url = format!("http://{}", server_address);
    let base_url = std::env::var("PHOTO_SERVER_BASE_URL").unwrap_or_else(|_| server_url.clone());
    let document_root = std::env::var("PHOTO_SERVER_DOCUMENT_ROOT").unwrap_or_else(|_| DOCUMENT_ROOT.to_owned());
    let thumbnail_root = std::env::var("PHOTO_SERVER_THUMBNAIL_ROOT").unwrap_or_else(|_| THUMBNAIL_ROOT.to_owned());
    let server = PhotoServer::new(&server_address, &base_url, &document_root, &thumbnail_root);
    println!("Listening on {}", server_url);
    server.handle_requests();
}
