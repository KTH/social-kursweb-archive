use anyhow::{bail, Context, Result};
use encoding::all::ISO_8859_1;
use encoding::{DecoderTrap, Encoding};
use lazy_regex::regex_is_match;
use serde::Deserialize;
use slug::slugify;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::prelude::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::from_utf8;
use xml::{writer::XmlEvent, EmitterConfig, EventWriter};

fn main() -> Result<()> {
    let path = PathBuf::from("../out");
    fs::create_dir_all(&path)?;
    let mut metaxml = EventWriter::new_with_config(
        File::create(path.join("leverans.xml"))
            .context("Failed to create leverans.xml")?,
        EmitterConfig::new().perform_indent(true),
    );
    metaxml.write(
        XmlEvent::start_element("Leveransobjekt")
            .ns("", "Schema_social")
            .ns("xsi", "http://www.w3.org/2001/XMLSchema-instance")
            .attr("xsi:schemaLocation", "Schema_social schema-social.xsd"),
    )?;
    metaxml.write(XmlEvent::start_element("SystemNamn"))?;
    metaxml.write(XmlEvent::characters("Social"))?;
    metaxml.write(XmlEvent::end_element())?;
    let srcpath: &Path = "../courses-1/data/".as_ref();
    for ltr in srcpath.read_dir()? {
        let ltr = ltr?.file_name();
        for code in srcpath.join(&ltr).read_dir()? {
            let code = code?.file_name();
            writecourse(
                &mut metaxml,
                srcpath,
                &path,
                ltr.as_ref(),
                code.to_str().unwrap(),
            )
            .with_context(|| format!("Handling {code:?}"))?;
        }
    }
    metaxml.write(XmlEvent::end_element())?;
    Ok(())
}

fn writecourse<W: Write>(
    metaxml: &mut EventWriter<W>,
    src: &Path,
    dest: &Path,
    base: &Path,
    code: &str,
) -> Result<()> {
    let srcbase = src.join(base).join(code);
    let data_path = srcbase.join("00-pages.json");
    let data: Vec<Node> = serde_json::from_reader(
        File::open(&data_path)
            .with_context(|| format!("Failed to open {data_path:?}"))?,
    )
    .with_context(|| format!("Failed to parse {data_path:?}"))?;

    let mut data = data
        .into_iter()
        .filter_map(|data| match data.into_node2(&srcbase) {
            Ok(data) => match data.is_relevant() {
                Ok(true) => Some(Ok(data)),
                Ok(false) => None,
                Err(err) => Some(Err(err)),
            },
            Err(err) => Some(Err(err)),
        })
        .collect::<Result<Vec<_>>>()?;
    if data.is_empty() {
        return Ok(()); // Nothing to arhive here!
    }

    let dest = dest.join(base).join(code);
    fs::create_dir_all(&dest)?;
    metaxml.write(XmlEvent::start_element("Kurs"))?;
    metaxml.write(XmlEvent::start_element("Kurskod"))?;
    metaxml.write(XmlEvent::characters(code))?;
    metaxml.write(XmlEvent::end_element())?;
    metaxml.write(XmlEvent::start_element("Kursnamn").attr("Lang", "sv"))?;
    metaxml.write(XmlEvent::characters("TODO"))?;
    metaxml.write(XmlEvent::end_element())?;

    metaxml.write(XmlEvent::start_element("Innehall"))?;
    for node in &mut data {
        node.handle(metaxml, &dest, &base.join(code))
            .with_context(|| format!("Handling node {:?}", &node.slug))?;
    }
    metaxml.write(XmlEvent::end_element())?;
    metaxml.write(XmlEvent::end_element())?;
    Ok(())
}

#[derive(Deserialize)]
struct Node {
    slug: String,
    last_modified: Modification,
    links: Vec<Link>,
}

impl Node {
    fn into_node2(self, srcbase: &Path) -> Result<Node2> {
        let files = self
            .links
            .iter()
            .filter(|link| link.is_file())
            .filter_map(|link| {
                link.get_file(srcbase).map_err(|e| eprintln!("{e:?}")).ok()
            })
            .collect::<Vec<_>>();
        let filename = format!("{}.html", self.slug);
        Ok(Node2 {
            slug: self.slug,
            doc: fs::read_to_string(srcbase.join(&filename))?,
            last_modified: self.last_modified,
            files,
        })
    }
}

struct Node2 {
    slug: String,
    doc: String,
    last_modified: Modification,
    files: Vec<FileNode>,
}

impl Node2 {
    fn is_relevant(&self) -> Result<bool> {
        Ok(is_relevant(&self.doc)
            || try_any(&self.files, FileNode::is_relevant)?)
    }
    fn handle<W: Write>(
        &mut self,
        metaxml: &mut EventWriter<W>,
        dest: &Path,
        dir: &Path,
    ) -> Result<()> {
        let filename = format!("{}.html", self.slug);
        metaxml.write(
            XmlEvent::start_element("Nod")
                .attr("Lank", ps(&dir.join(&filename))?)
                // .attr("Skapad", todo!()) (första datum finns inte i min json, måste i så fall dumpas om från källan.
                .attr("Andrad", &self.last_modified.time),
        )?;
        for link in &self.files {
            let data = fs::read(&link.path);
            match data {
                Err(e) => {
                    eprintln!("In {dir:?}, skipping Bilaga {link:?}: {e}")
                }
                Ok(data) => {
                    let destname = link.destname();
                    write(&dest.join(&destname), &data)?;
                    let mut ndoc = self.doc.replace(&link.srcname, &destname);
                    std::mem::swap(&mut self.doc, &mut ndoc);
                    metaxml.write(
                        XmlEvent::start_element("Bilaga")
                            .attr("Lank", ps(&dir.join(&destname))?)
                            .attr(
                                "Filnamn",
                                // TODO? link.filename()
                                &destname,
                            )
                            .attr("Storlek", &data.len().to_string()),
                        // .attr("Skapad", todo!()) (första datum finns inte i min json, måste i så fall dumpas om från källan.
                        // .attr("Ändrad", &node.last_modified.time)
                    )?;
                    metaxml.write(XmlEvent::end_element())?;
                }
            }
        }
        metaxml.write(XmlEvent::end_element())?;
        write(&dest.join(&filename), &self.doc)?;
        Ok(())
    }
}

fn try_any<T, Cond>(links: &[T], cond: Cond) -> Result<bool>
where
    Cond: Fn(&T) -> Result<bool>,
{
    for item in links {
        if cond(item)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Deserialize)]
struct Modification {
    time: String,
}

#[derive(Debug, Deserialize)]
struct Link {
    url: String,
    category: Option<String>,
}

impl Link {
    fn is_file(&self) -> bool {
        match self.category.as_deref() {
            Some("file") | None => true,
            Some("ext") => false,
            Some("incourse") => false,
            Some(category) => panic!("Unknown category {category:?}"),
        }
    }

    fn get_file(&self, srcbase: &Path) -> Result<FileNode> {
        let path = srcbase.join(&self.url);
        let srcname = self.url.clone();
        if path.exists() {
            return Ok(FileNode { path, srcname });
        }
        let path = srcbase.join(&self.url.replace('+', "%20"));
        if path.exists() {
            return Ok(FileNode { path, srcname });
        }
        let path = srcbase.join(OsStr::from_bytes(
            urlencoding::decode_binary(self.url.as_ref()).as_ref(),
        ));
        if path.exists() {
            return Ok(FileNode { path, srcname });
        }
        let path = srcbase.join({
            self.url
                .rsplit_once('/')
                .map(|(dir, file)| {
                    format!("{}/{}", dir, urlencoding::encode(file).as_ref())
                })
                .unwrap()
        });
        if path.exists() {
            return Ok(FileNode { path, srcname });
        }
        bail!("Failed to find path {srcname:?} in {srcbase:?}.");
    }

    /// The original name of the file, if that is of interest.
    #[allow(unused)]
    fn filename(&self) -> &str {
        self.url
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(&self.url)
    }
}

fn is_relevant(doc: &str) -> bool {
    regex_is_match!(
        r"\b((om)?tenta|assign?e?ment|lab|[öo]vning|l[äa]xa|inl[äa]mning|munta|quiz|examination|uppgift|seminar|facit|(kontroll|sals?)skrivning|formelsamling)\b"i,
        doc
    )
}

fn ps(path: &Path) -> Result<&str> {
    path.to_str().context("non-utf8 path")
}

fn write<D: AsRef<[u8]>>(path: &Path, data: D) -> Result<()> {
    fs::write(path, data).with_context(|| format!("Failed to write {path:?}"))
}

#[derive(Debug)]
struct FileNode {
    path: PathBuf,
    srcname: String,
}

impl FileNode {
    fn is_relevant(&self) -> Result<bool> {
        let ext = self
            .path
            .extension()
            .map(|s| s.to_ascii_lowercase().to_string_lossy().into_owned());
        match ext.as_deref() {
            Some("doc" | "docx") => Ok(false), // word. Maybe extract data here?
            Some("dxf") => Ok(false), // autocad? Maybe extract data here?
            Some("idml") => Ok(false), // indesign. Maybe extract data here?
            Some("indd") => Ok(false), // indesign. Maybe extract data here?
            Some("jpg" | "jpeg" | "png" | "tif" | "tiff") => Ok(false), // image
            Some("mp3" | "wav") => Ok(false), // sound
            Some("odt") => Ok(false), // open document. Maybe extract data here?
            Some("ppt" | "pptx") => Ok(false), // Maybe extract data here?
            Some("webarchive") => Ok(false), // apple junk. Maybe extract data here?
            Some("xls" | "xlsx") => Ok(false), // Maybe extract data here?
            Some("zip") => Ok(false), // Archive. Maybe extract data here?
            Some("pdf" | "ai") => {
                let result = Command::new("pdftotext")
                    .arg(&self.path)
                    .arg("-")
                    .output()
                    .context("extract pdf text")?;
                if !result.status.success() {
                    let err = result.stderr;
                    if err == b"Syntax Error: Document stream is empty\n"
                        || err == b"Command Line Error: Incorrect password\n"
                    {
                        return Ok(false); // empty or encrypted files are not relevant
                    } else {
                        eprintln!(
                            "pdftotext failed for {:?}: {:?}",
                            self.path,
                            from_utf8(&err)
                        );
                        return Ok(false);
                    }
                }
                Ok(is_relevant(from_utf8(&result.stdout)?))
            }
            _ => {
                let data = fs::read(&self.path)
                    .with_context(|| format!("Reading {:?}", self.path))?;
                Ok(is_relevant(&String::from_utf8_lossy(&data)))
            }
        }
    }

    fn destname(&self) -> String {
        let data = self.srcname.trim_start_matches("01-files/");
        let data = if data.contains('%') {
            String::from_utf8(
                urlencoding::decode_binary(data.as_ref()).into_owned(),
            )
            .unwrap_or_else(|failed| {
                ISO_8859_1
                    .decode(failed.as_bytes(), DecoderTrap::Replace)
                    .unwrap()
            })
        } else {
            data.into()
        };
        if let Some((name, suffix)) = data.rsplit_once('.') {
            format!("{}.{}", slugify(name), suffix)
        } else {
            slugify(data)
        }
    }
}
