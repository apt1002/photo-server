use std::{fmt};
use std::error::{Error};
use std::ffi::{OsStr, OsString};
use std::fs::{File};

use tiny_http::{Method, Request, Response, Header};

pub use html_escape::{encode_text as html_escape};
pub use url::{Url};

/// Given `"foo.BAR"` and `"bar"` returns `Some("foo")`.
pub fn remove_extension<'a>(filename: &'a str, extension: &str) -> Option<&'a str> {
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
///
/// There are perfectly good filenames that do not satisfy this criterion, but
/// those that do are unlikely to need to be escaped in any context. This
/// criterion is satisfied by many common filenames, including auto-generated
/// filenames that are based on dates, hashes or sequence numbers.
pub fn validate_name(s: &OsStr) -> Result<&str, DubiousFilename> {
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

/// A normal HTTP response.
// TODO: Redirect.
#[derive(Debug)]
pub enum HttpOkay {
    File(File),
    Html(String),
    Jpeg(Vec<u8>),
}

/// An erroneous HTTP response.
#[derive(Debug)]
pub enum HttpError {
    Invalid,
    NotFound,
    Error(Box<dyn Error>),
}

impl HttpError {
    pub fn new(e: impl 'static + Error) -> Self { Self::Error(e.into()) }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for HttpError {}

macro_rules! impl_httperror_from {
    ($e:ty) => {
        impl From<$e> for HttpError {
            fn from(e: $e) -> Self { HttpError::Error(e.into()) }
        }
    };
}

impl_httperror_from!(std::io::Error);
impl_httperror_from!(url::ParseError);
impl_httperror_from!(DubiousFilename);

// ----------------------------------------------------------------------------

/// Implement this to write your web application.
pub trait Handler {
    /// Represents the URL request parameters that are recognised by this Handler.
    type Params: FromIterator<(String, String)>;

    fn handle_get(
        &self,
        absolute_url: Url,
        path: Vec<String>,
        params: Self::Params,
    ) -> Result<HttpOkay, HttpError>;
}

// ----------------------------------------------------------------------------

struct Server<H: Handler> {
    /// Web server.
    pub server: tiny_http::Server,

    /// The external URL of the server.
    pub base_url: Url,

    /// The application-specific state.
    pub handler: H,
}

impl<H: Handler> Server<H> {
    fn new(addr: &str, base_url: &str, handler: H) -> Self {
        Server {
            server: tiny_http::Server::http(addr).expect("Could not create the web server"),
            base_url: url::Url::parse(base_url).expect("Could not parse the base URL"),
            handler,
        }
    }

    fn handle_request(&self, request: &mut Request) -> Result<HttpOkay, HttpError> {
        let absolute_url = self.base_url.join(request.url())?;
        println!("{} {}", request.remote_addr().unwrap().ip(), absolute_url);
        // Parse the query parameters.
        let params = absolute_url.query_pairs().map(
            |(key, value)| (
                url_escape::decode(key.as_ref()).into_owned(),
                url_escape::decode(value.as_ref()).into_owned(),
            )
        ).collect();
        // Parse the path segments.
        let mut path: Vec<String> = absolute_url.path_segments().ok_or(HttpError::Invalid)?.map(
            |s| url_escape::decode(s).into_owned()
        ).collect();
        if let Some(last) = path.last() {
            if "" == last { path.pop(); }
        }
        // Dispatch based on HTTP method.
        match request.method() {
            Method::Get => self.handler.handle_get(absolute_url, path, params),
            _ => Err(HttpError::Invalid),
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
    pub fn handle_requests(&self) -> ! {
        for mut request in self.server.incoming_requests() {
            match self.handle_request(&mut request) {
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
                Err(HttpError::Error(e)) => {
                    println!("Error: {}", e);
                    request.respond(Response::from_string("Server error").with_status_code(500))
                },
            }.unwrap_or_else(|e2| println!("IO Error: {}", e2));
        }
        unreachable!();
    }
}

pub fn start(server_address: String, base_url: Option<String>, handler: impl Handler) -> ! {
    let server_url = format!("http://{}", server_address);
    let base_url = base_url.unwrap_or_else(|| server_url.clone());
    let server = Server::new(&server_address, &base_url, handler);
    println!("Listening on {}", server_url);
    server.handle_requests();
}
