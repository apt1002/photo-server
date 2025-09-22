use std::collections::{HashMap};
use std::error::{Error};
// use std::io::{Write};
use std::fs::{File};
use std::path::{Path};
use std::str::{FromStr};

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

// ----------------------------------------------------------------------------

/// Represent HTTP request parameters.
#[derive(Debug)]
pub struct Params(HashMap<String, String>);

impl Params {
    fn get<T: FromStr>(&self, key: &str) -> Result<T, HttpError>
    where HttpError: From<<T as FromStr>::Err> {
        Ok(self.0.get(key).ok_or(HttpError::Invalid)?.parse::<T>()?)
    }

    fn get_optional<T: FromStr>(&self, key: &str, default: T) -> Result<T, HttpError>
    where HttpError: From<<T as FromStr>::Err> {
        if let Some(s) = self.0.get(key) {
            Ok(s.parse::<T>()?)
        } else {
            Ok(default)
        }
    }
}

// ----------------------------------------------------------------------------

/// Information about a user.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct Session{
    pub w: u32,
    pub h: u32,
}

impl Session {
    fn from_params(params: &Params) -> Result<Self, HttpError> {
        Ok(Self {
            w: params.get_optional("w", 800)?.min(2048),
            h: params.get_optional("h", 600)?.min(2048),
        })
    }

    fn to_params(&self) -> String { format!("w={}&h={}", self.w, self.h) }
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
        let params = Params(url.query_pairs().map(
            |(key, value)| (key.into_owned(), value.into_owned())
        ).collect());
        let mut path = url.path_segments().unwrap();
        let dir_name = path.next().ok_or(HttpError::Invalid)?;
        if let Some(leaf_name) = path.next() {
            if leaf_name.ends_with(".JPG") {
                self.rescale(leaf_name, &params)
            } else if leaf_name.ends_with(".JPG.html") {
                self.frame(leaf_name, &params)
            } else if leaf_name.ends_with(".JPG.thumb") {
                self.thumb(leaf_name, &params)
            } else {
                println!("Not found: {:?}", leaf_name);
                Err(HttpError::NotFound)
            }
        } else {
            self.index(&params)
        }
    }

    /// Show thumbnails for all photos in a directory.
    pub fn index(&self, params: &Params) -> Result<HttpOkay, HttpError> {
        Err(HttpError::Invalid)
    }

    /// Serve a resized JPEG file.
    pub fn rescale(&self, leaf_name: &str, params: &Params) -> Result<HttpOkay, HttpError> {
        Err(HttpError::Invalid)
    }
    
    /// Show an HTML frame around a single photo.
    pub fn frame(&self, leaf_name: &str, params: &Params) -> Result<HttpOkay, HttpError> {
        Err(HttpError::Invalid)
    }

    /// Serve a JPEG thumbnail.
    pub fn thumb(&self, leaf_name: &str, params: &Params) -> Result<HttpOkay, HttpError> {
        Err(HttpError::Invalid)
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
