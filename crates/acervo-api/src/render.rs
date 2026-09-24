//! Documentos XML da superfície Torznab: capacidades, resultados e erro.

use std::collections::BTreeMap;
use std::io;

use acervo_indexers::{Capabilities, Category, Release, SearchSupport};
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesText, Event};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;

use crate::TorznabError;
use crate::request::MAX_RESULTS;

const TORZNAB_NS: &str = "http://torznab.com/schemas/2015/feed";

/// Documento de `t=caps`.
#[must_use]
pub fn capabilities(caps: &Capabilities) -> String {
    document(|writer| {
        writer
            .create_element("caps")
            .write_inner_content(|writer| {
                writer
                    .create_element("server")
                    .with_attribute(("title", "acervo-hub"))
                    .write_empty()?;
                let max = MAX_RESULTS.to_string();
                writer
                    .create_element("limits")
                    .with_attributes([("max", max.as_str()), ("default", max.as_str())])
                    .write_empty()?;
                writer
                    .create_element("searching")
                    .write_inner_content(|writer| {
                        mode(writer, "search", &caps.general)?;
                        mode(writer, "tv-search", &caps.tv)?;
                        mode(writer, "movie-search", &caps.movie)
                    })?;
                writer
                    .create_element("categories")
                    .write_inner_content(|writer| categories(writer, &caps.categories))?;
                Ok(())
            })?;
        Ok(())
    })
}

fn mode<W: io::Write>(
    writer: &mut Writer<W>,
    name: &str,
    support: &SearchSupport,
) -> io::Result<()> {
    let params = support
        .supported_params
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    writer
        .create_element(name)
        .with_attributes([
            ("available", if support.available { "yes" } else { "no" }),
            ("supportedParams", params.as_str()),
        ])
        .write_empty()?;
    Ok(())
}

/// Árvore de duas camadas que o formato exige: filhas dentro da mãe.
///
/// Filha cuja mãe não foi anunciada ganha uma mãe sintética — solta no nível
/// de cima, ela seria lida como categoria-mãe e cobriria o grupo inteiro.
fn categories<W: io::Write>(writer: &mut Writer<W>, all: &[Category]) -> io::Result<()> {
    let mut tree: BTreeMap<u32, (String, Vec<&Category>)> = BTreeMap::new();
    for category in all.iter().filter(|category| category.parent.is_none()) {
        tree.entry(category.id)
            .or_insert_with(|| (category.name.clone(), Vec::new()));
    }
    for category in all {
        if let Some(parent) = category.parent {
            tree.entry(parent)
                .or_insert_with(|| (parent_name(category, parent), Vec::new()))
                .1
                .push(category);
        }
    }
    for (id, (name, children)) in tree {
        let id = id.to_string();
        writer
            .create_element("category")
            .with_attributes([("id", id.as_str()), ("name", name.as_str())])
            .write_inner_content(|writer| {
                for child in children {
                    let child_id = child.id.to_string();
                    writer
                        .create_element("subcat")
                        .with_attributes([("id", child_id.as_str()), ("name", child.name.as_str())])
                        .write_empty()?;
                }
                Ok(())
            })?;
    }
    Ok(())
}

fn parent_name(child: &Category, parent: u32) -> String {
    child
        .name
        .split_once('/')
        .map_or_else(|| parent.to_string(), |(head, _)| head.to_owned())
}

/// Feed RSS de resultados.
///
/// Release sem data sai com `now`: os consumidores recusam o feed inteiro
/// quando um item não tem `pubDate`, e uma data ausente não justifica perder
/// os outros resultados.
#[must_use]
///
/// `link` decide o endereço de download de cada release — direto, ou por
/// este serviço quando o indexador exige sessão.
pub fn releases(
    title: &str,
    releases: &[Release],
    now: OffsetDateTime,
    link: &dyn Fn(&Release) -> String,
) -> String {
    document(|writer| {
        writer
            .create_element("rss")
            .with_attributes([
                ("version", "2.0"),
                ("xmlns:atom", "http://www.w3.org/2005/Atom"),
                ("xmlns:torznab", TORZNAB_NS),
            ])
            .write_inner_content(|writer| {
                writer
                    .create_element("channel")
                    .write_inner_content(|writer| {
                        text(writer, "title", title)?;
                        text(writer, "description", "acervo-hub")?;
                        for release in releases {
                            item(writer, release, now, &link(release))?;
                        }
                        Ok(())
                    })?;
                Ok(())
            })?;
        Ok(())
    })
}

fn item<W: io::Write>(
    writer: &mut Writer<W>,
    release: &Release,
    now: OffsetDateTime,
    download: &str,
) -> io::Result<()> {
    let published = release
        .published
        .unwrap_or(now)
        .format(&Rfc2822)
        .map_err(io::Error::other)?;
    let size = release.size.to_string();
    writer
        .create_element("item")
        .write_inner_content(|writer| {
            text(writer, "title", &release.title)?;
            writer
                .create_element("guid")
                .with_attribute(("isPermaLink", "false"))
                .write_text_content(BytesText::new(&release.guid))?;
            if let Some(info) = &release.info_url {
                text(writer, "comments", info.as_str())?;
            }
            text(writer, "pubDate", &published)?;
            text(writer, "size", &size)?;
            text(writer, "link", download)?;
            writer
                .create_element("enclosure")
                .with_attributes([
                    ("url", download),
                    ("length", size.as_str()),
                    ("type", "application/x-bittorrent"),
                ])
                .write_empty()?;
            for category in &release.categories {
                text(writer, "category", &category.to_string())?;
            }
            for category in &release.categories {
                attr(writer, "category", &category.to_string())?;
            }
            if release.download_url.scheme() == "magnet" {
                attr(writer, "magneturl", download)?;
            }
            if let Some(seeders) = release.seeders {
                attr(writer, "seeders", &seeders.to_string())?;
                let peers = u64::from(seeders) + u64::from(release.leechers.unwrap_or(0));
                attr(writer, "peers", &peers.to_string())?;
            }
            if let Some(grabs) = release.grabs {
                attr(writer, "grabs", &grabs.to_string())?;
            }
            Ok(())
        })?;
    Ok(())
}

/// Documento de erro. Os consumidores leem o `code` para decidir se o
/// problema é de credencial, de parâmetro ou do indexador.
#[must_use]
pub fn error(error: &TorznabError) -> String {
    let code = error.code().to_string();
    let description = error.to_string();
    document(|writer| {
        writer
            .create_element("error")
            .with_attributes([
                ("code", code.as_str()),
                ("description", description.as_str()),
            ])
            .write_empty()?;
        Ok(())
    })
}

fn text<W: io::Write>(writer: &mut Writer<W>, name: &str, value: &str) -> io::Result<()> {
    writer
        .create_element(name)
        .write_text_content(BytesText::new(value))?;
    Ok(())
}

fn attr<W: io::Write>(writer: &mut Writer<W>, name: &str, value: &str) -> io::Result<()> {
    writer
        .create_element("torznab:attr")
        .with_attributes([("name", name), ("value", value)])
        .write_empty()?;
    Ok(())
}

fn document(body: impl FnOnce(&mut Writer<Vec<u8>>) -> io::Result<()>) -> String {
    let mut writer = Writer::new(Vec::new());
    // Escrever num `Vec` só falha se a data não formatar, e toda data que
    // chega aqui veio de um `OffsetDateTime` válido.
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .and_then(|()| body(&mut writer))
        .expect("escrever XML em memória não falha");
    String::from_utf8(writer.into_inner()).expect("quick-xml só escreve UTF-8")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn release(download: &str) -> Release {
        Release {
            indexer: "publico".into(),
            guid: "https://tracker.invalid/details/1".into(),
            title: "Uma.Série.S01E02 <1080p> & mais".into(),
            download_url: url::Url::parse(download).unwrap(),
            info_url: Some(url::Url::parse("https://tracker.invalid/details/1").unwrap()),
            size: 1_610_612_736,
            published: None,
            seeders: Some(12),
            leechers: Some(3),
            grabs: None,
            categories: vec![5040],
            tags: Vec::new(),
        }
    }

    #[test]
    fn feed_escapa_titulo_e_preenche_data_ausente() {
        let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
        let xml = releases(
            "publico",
            &[release("https://tracker.invalid/dl/1?passkey=abc&x=1")],
            now,
            &|release| release.download_url.to_string(),
        );

        assert!(xml.contains("<title>Uma.Série.S01E02 &lt;1080p&gt; &amp; mais</title>"));
        assert!(xml.contains("<pubDate>Thu, 01 Jan 1970 00:00:00 +0000</pubDate>"));
        assert!(xml.contains("passkey=abc&amp;x=1"));
        assert!(xml.contains(r#"<torznab:attr name="peers" value="15"/>"#));
        assert!(xml.contains(r#"length="1610612736""#));
        assert!(!xml.contains("magneturl"));
    }

    #[test]
    fn magnet_vai_tambem_como_atributo() {
        let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
        let xml = releases(
            "p",
            &[release("magnet:?xt=urn:btih:abc")],
            now,
            &|release| release.download_url.to_string(),
        );
        assert!(
            xml.contains(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:abc"/>"#)
        );
    }

    #[test]
    fn filha_sem_mae_anunciada_ganha_mae_sintetica() {
        let caps = Capabilities {
            general: SearchSupport {
                available: true,
                supported_params: BTreeSet::from(["q".to_owned()]),
            },
            categories: vec![
                Category {
                    id: 5040,
                    name: "TV/HD".into(),
                    parent: Some(5000),
                },
                Category {
                    id: 2000,
                    name: "Movies".into(),
                    parent: None,
                },
            ],
            ..Capabilities::default()
        };
        let xml = capabilities(&caps);

        assert!(xml.contains(r#"<search available="yes" supportedParams="q"/>"#));
        assert!(xml.contains(r#"<tv-search available="no" supportedParams=""/>"#));
        assert!(xml.contains(
            r#"<category id="5000" name="TV"><subcat id="5040" name="TV/HD"/></category>"#
        ));
        assert!(xml.contains(r#"<category id="2000" name="Movies"></category>"#));
    }

    #[test]
    fn erro_leva_codigo_do_contrato() {
        let xml = error(&TorznabError::IncorrectApiKey);
        assert!(xml.contains(r#"<error code="100""#));
    }
}
