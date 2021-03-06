use anyhow::{anyhow, ensure, Context, Result};
use encoding::all::ISO_8859_1;
use encoding::{DecoderTrap, Encoding};
use lazy_regex::regex_is_match;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use slug::slugify;
use std::collections::BTreeMap;
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
        File::create(path.join("social.xml"))
            .context("Failed to create social.xml")?,
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
    let srcpath: &Path = "data".as_ref();
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
    let data = read_json::<Vec<Node>>(&srcbase.join("00-pages.json"))?;
    let info = read_json::<Info>(&srcbase.join("00-info.json"))?;
    ensure!(code == &info.code);

    let mut nodes = Vec::new();
    let mut groups: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for item in data {
        let (group, data) = item.into_node2(&srcbase)?;
        if data.is_relevant()? {
            if let Some(group) = group {
                groups.entry(group).or_default().push(data);
            } else {
                nodes.push(data);
            }
        }
    }
    if nodes.is_empty() && groups.is_empty() {
        return Ok(()); // Nothing to arhive here!
    }

    let dest = dest.join(base).join(code);
    fs::create_dir_all(&dest)?;
    metaxml.write(XmlEvent::start_element("Kurs"))?;
    metaxml.write(XmlEvent::start_element("Kurskod"))?;
    metaxml.write(XmlEvent::characters(code))?;
    metaxml.write(XmlEvent::end_element())?;
    for (lang, name) in &info.name {
        metaxml
            .write(XmlEvent::start_element("Kursnamn").attr("Lang", lang))?;
        metaxml.write(XmlEvent::characters(name))?;
        metaxml.write(XmlEvent::end_element())?;
    }

    writecontent(metaxml, nodes, &dest, &base.join(code))?;
    for (group, nodes) in groups {
        metaxml.write(XmlEvent::start_element("Kurstillfalle"))?;
        metaxml.write(XmlEvent::start_element("Kurstillfalleskod"))?;
        metaxml.write(XmlEvent::characters(&group))?;
        metaxml.write(XmlEvent::end_element())?;
        writecontent(metaxml, nodes, &dest, &base.join(code))?;
        metaxml.write(XmlEvent::end_element())?;
    }
    metaxml.write(XmlEvent::end_element())?;
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(
        File::open(&path)
            .with_context(|| format!("Failed to open {path:?}"))?,
    )
    .with_context(|| format!("Failed to parse {path:?}"))
}

fn writecontent<W: Write>(
    metaxml: &mut EventWriter<W>,
    nodes: Vec<Node2>,
    dest: &Path,
    base: &Path,
) -> Result<()> {
    if !nodes.is_empty() {
        metaxml.write(XmlEvent::start_element("Innehall"))?;
        for mut node in nodes {
            node.handle(metaxml, &dest, &base)
                .with_context(|| format!("Handling node {:?}", &node.slug))?;
        }
        metaxml.write(XmlEvent::end_element())?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct Info {
    code: String,
    name: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Node {
    slug: String,
    created_time: String,
    last_modified: Modification,
    links: Vec<Link>,
    roundgroup: Option<String>,
}

impl Node {
    fn into_node2(self, srcbase: &Path) -> Result<(Option<String>, Node2)> {
        let files = self
            .links
            .iter()
            .filter(|link| link.is_file())
            .filter_map(|link| {
                link.get_file(srcbase).map_err(|e| eprintln!("{e:?}")).ok()
            })
            .collect::<Vec<_>>();
        let filename = format!("{}.html", self.slug);
        Ok((
            self.roundgroup,
            Node2 {
                slug: self.slug,
                doc: fs::read_to_string(srcbase.join(&filename))?,
                created_time: self.created_time,
                last_modified: self.last_modified,
                files,
            },
        ))
    }
}

struct Node2 {
    slug: String,
    doc: String,
    created_time: String,
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
                .attr("Skapad", &self.created_time)
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
                    let link_attr = dir.join(&destname);
                    let len_attr = data.len().to_string();
                    let event = XmlEvent::start_element("Bilaga")
                        .attr("Lank", ps(&link_attr)?)
                        .attr(
                            "Filnamn",
                            // TODO? link.filename()
                            &destname,
                        )
                        .attr("Storlek", &len_attr);
                    let event =
                        if let Some(created) = link.created_time.as_ref() {
                            event.attr("Uppladdningsdatum", created)
                        } else {
                            event
                        };
                    metaxml.write(event)?;
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
    created_time: Option<String>,
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
        let url = self.url.replace("/social/upload/", "01-files/");
        self.check_path_file(srcbase.join(&url))
            .or_else(|| {
                self.check_path_file(srcbase.join(&url.replace('+', "%20")))
            })
            .or_else(|| {
                self.check_path_file(srcbase.join(&url.replace("%2B", "%20")))
            })
            .or_else(|| {
                self.check_path_file(srcbase.join(OsStr::from_bytes(
                    urlencoding::decode_binary(url.as_ref()).as_ref(),
                )))
            })
            .or_else(|| {
                url.rsplit_once('/').and_then(|(dir, file)| {
                    self.check_path_file(
                        srcbase
                            .join(dir)
                            .join(urlencoding::encode(file).as_ref()),
                    )
                })
            })
            .ok_or_else(|| {
                anyhow!("Failed to find path {url:?} in {srcbase:?}.")
            })
    }

    fn check_path_file(&self, path: PathBuf) -> Option<FileNode> {
        if path.exists() {
            Some(FileNode {
                path,
                created_time: self.created_time.clone(),
                srcname: self.url.clone(),
            })
        } else {
            None
        }
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
        r"\b((om)?tenta|assign?e?ment|lab|[??o]vning|l[??a]xa|inl[??a]mning|munta|quiz|examination|uppgift|seminar|facit|(kontroll|sals?)skrivning|formelsamling)\b"i,
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
    created_time: Option<String>,
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
            Some("jpg" | "jpeg" | "png" | "tif" | "tiff" | "webm") => Ok(false),
            Some("mp3" | "wav") => Ok(false), // sound
            Some("odt") => Ok(false), // open document. Maybe extract data here?
            Some("pcap") => Ok(false), // network dump
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
