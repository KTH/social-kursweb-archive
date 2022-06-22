use anyhow::{ensure, Context, Result};
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
    let dest = dest.join(base).join(code);

    // TODO: Don't create dir or write element if nothing in the course!
    fs::create_dir_all(&dest)?;
    metaxml.write(XmlEvent::start_element("Kurs"))?;
    metaxml.write(XmlEvent::start_element("Kurskod"))?;
    metaxml.write(XmlEvent::characters(code))?;
    metaxml.write(XmlEvent::end_element())?;
    metaxml.write(XmlEvent::start_element("Kursnamn").attr("Lang", "sv"))?;
    metaxml.write(XmlEvent::characters("TODO"))?;
    metaxml.write(XmlEvent::end_element())?;

    let data_path = srcbase.join("00-pages.json");
    let mut data: Vec<Node> = serde_json::from_reader(
        File::open(&data_path)
            .with_context(|| format!("Failed to open {data_path:?}"))?,
    )
    .with_context(|| format!("Failed to parse {data_path:?}"))?;

    metaxml.write(XmlEvent::start_element("Innehall"))?;
    for node in &mut data {
        node.handle(metaxml, &srcbase, &dest, &base.join(code))
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
    fn handle<W: Write>(
        &mut self,
        metaxml: &mut EventWriter<W>,
        srcbase: &Path,
        dest: &Path,
        dir: &Path,
    ) -> Result<()> {
        let filename = format!("{}.html", self.slug);
        let mut doc = fs::read_to_string(srcbase.join(&filename))?;

        self.links.retain(|link| link.is_file());

        if is_relevant(&doc)
            || try_any(&self.links, |link: &Link| link.is_relevant(srcbase))?
        {
            metaxml.write(
                XmlEvent::start_element("Nod")
                    .attr("Lank", ps(&dir.join(&filename))?)
                    // .attr("Skapad", todo!()) (första datum finns inte i min json, måste i så fall dumpas om från källan.
                    .attr("Andrad", &self.last_modified.time),
            )?;
            for link in &self.links {
                let data = fs::read(srcbase.join(&link.url))
                    .or_else(|_| {
                        fs::read(srcbase.join(&link.url.replace('+', "%20")))
                    })
                    .or_else(|_| {
                        fs::read(
                            srcbase.join(OsStr::from_bytes(
                                urlencoding::decode_binary(link.url.as_ref())
                                    .as_ref(),
                            )),
                        )
                    })
                    .or_else(|_| {
                        fs::read(srcbase.join({
                            link.url
                                .rsplit_once('/')
                                .map(|(dir, file)| {
                                    format!(
                                        "{}/{}",
                                        dir,
                                        urlencoding::encode(file).as_ref()
                                    )
                                })
                                .unwrap()
                        }))
                    });
                match data {
                    Err(e) => {
                        eprintln!("In {dir:?}, skipping Bilaga {link:?}: {e}")
                    }
                    Ok(data) => {
                        let destname = link.destname();
                        write(&dest.join(&destname), &data)?;
                        let mut ndoc = doc.replace(&link.url, &destname);
                        std::mem::swap(&mut doc, &mut ndoc);
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
            write(&dest.join(&filename), doc)?;
        }
        Ok(())
    }
}

fn try_any<Cond>(links: &[Link], cond: Cond) -> Result<bool>
where
    Cond: Fn(&Link) -> Result<bool>,
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
    category: String,
}

impl Link {
    fn is_file(&self) -> bool {
        match self.category.as_str() {
            "file" => true,
            "ext" => false,
            "incourse" => false,
            category => panic!("Unknown category {category:?}"),
        }
    }

    fn is_relevant(&self, base: &Path) -> Result<bool> {
        let path = base.join(&self.url);
        match path.extension().map(|s| s.to_str().unwrap()) {
            Some("jpg") => Ok(false),
            Some("png") => Ok(false),
            Some("pdf") => {
                let result = Command::new("pdftotext")
                    .arg(path)
                    .arg("-")
                    .output()
                    .context("extract pdf text")?;
                ensure!(result.status.success());
                Ok(is_relevant(from_utf8(&result.stdout)?))
            }
            _ => {
                let data = fs::read_to_string(&path)
                    .with_context(|| format!("Reading {path:?}"))?;
                Ok(is_relevant(&data))
            }
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

    fn destname(&self) -> String {
        let data = self.url.trim_start_matches("01-files/");
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

fn is_relevant(doc: &str) -> bool {
    regex_is_match!(
        r"\b(tenta|assign?e?ment|lab|[öo]vning)?|l[äa]xa|inl[äa]mning|munta|quiz|examination|uppgift|seminar|facit|(kontroll|sals?)skrivning|formelsamling"i,
        doc
    )
}

fn ps(path: &Path) -> Result<&str> {
    path.to_str().context("non-utf8 path")
}

fn write<D: AsRef<[u8]>>(path: &Path, data: D) -> Result<()> {
    fs::write(path, data).with_context(|| format!("Failed to write {path:?}"))
}
