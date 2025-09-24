use std::{env, fmt};
use std::fs::{File};
use std::io::{Read, Write};
use std::path::{Path};

mod server;
use server::{Handler, HttpOkay, HttpError, html_escape, Url, remove_extension, validate_name};

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

    /// Given one of the filenames in `self.jpegs`, returns the previous and
    /// next such filename.
    fn previous_next(&self, jpeg_name: &str) -> Option<(&str, &str)> {
        if let Some(prev) = self.jpegs.last() {
            let mut prev: &str = &prev;
            let mut iter = self.jpegs.iter();
            while let Some(p) = iter.next() {
                if p == jpeg_name {
                    let next: &str = iter.next().unwrap_or(self.jpegs.first().unwrap());
                    return Some((prev, next));
                }
                prev = p;
            }
        }
        None
    }
}

// ----------------------------------------------------------------------------

struct PhotoServer<'a> {
    /// The directory containing the photos.
    pub document_root: &'a Path,

    /// The thumbnail cache directory.
    pub thumbnail_root: &'a Path,
}

impl<'a> PhotoServer<'a> {
    fn new(document_root: &'a str, thumbnail_root: &'a str) -> Self {
        Self {
            document_root: Path::new(document_root),
            thumbnail_root: Path::new(thumbnail_root),
        }
    }

    /// Load `jpeg_name`, resize it, and encode it as a new JPEG file.
    fn resize_jpeg(jpeg_name: &Path, d: Dimensions) -> Result<Vec<u8>, HttpError> {
        let image = image::open(jpeg_name).map_err(HttpError::new)?;
        let image = image.resize(d.w, d.h, image::imageops::FilterType::Lanczos3);
        let mut ret = Vec::<u8>::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut ret, 85);
        encoder.encode_image(&image).map_err(HttpError::new)?;
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
                text = html_escape(&text),
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
        // `previous` and `next` links.
        let album = Album::new(&self.document_root.join(dir_name))?;
        let (previous, next) = album.previous_next(leaf_name).ok_or(HttpError::NotFound)?;
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
            previous = previous,
            next = next,
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
}

impl<'a> Handler for PhotoServer<'a> {
    type Params = Params;

    /// Handle a single request.
    fn handle_get(
        &self,
        _absolute_url: Url,
        path: Vec<String>,
        params: Self::Params,
    ) -> Result<HttpOkay, HttpError> {
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
}

// ----------------------------------------------------------------------------

/// Where the photo albums are.
const DOCUMENT_ROOT: &'static str = "./document_root";

/// Where we can cache thumbnails.
const THUMBNAIL_ROOT: &'static str = "./thumbnail_root";

/// The default server address and port to listen on.
const SERVER_ADDRESS: &'static str = "127.0.0.1:8082";

fn main() {
    // Application-specific part.
    let document_root = env::var("PHOTO_SERVER_DOCUMENT_ROOT").unwrap_or_else(|_| DOCUMENT_ROOT.to_owned());
    let thumbnail_root = env::var("PHOTO_SERVER_THUMBNAIL_ROOT").unwrap_or_else(|_| THUMBNAIL_ROOT.to_owned());
    let photo_server = PhotoServer::new(&document_root, &thumbnail_root);
    // Web server part.
    let server_address = env::var("PHOTO_SERVER_ADDRESS").unwrap_or_else(|_| SERVER_ADDRESS.to_owned());
    let base_url = env::var("PHOTO_SERVER_BASE_URL").ok();
    // Run for ever!
    server::start(server_address, base_url, photo_server);
}
