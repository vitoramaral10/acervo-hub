//! Envio ao Amigos pelo formulário do próprio site, com a sessão do
//! indexador.
//!
//! O formulário (`/torrents/upload`, Laravel com Inertia) faz, no navegador:
//!
//! 1. a busca no IMDB (`/torrents/lookup`), que devolve título, gêneros,
//!    idioma original e a capa já hospedada no site;
//! 2. a leitura do `MediaInfo` (`/torrents/mediainfo`), que sugere, por nome,
//!    a opção de cada lista (qualidade, áudio, codecs, extensão);
//! 3. o upload de cada captura (`/torrents/screenshots`);
//! 4. o `POST /torrents` multipart com tudo.
//!
//! O acervo refaz os mesmos passos. Os ids das opções não são fixos aqui:
//! vêm das props da página a cada envio, e a escolha é a do próprio site
//! (nome normalizado igual ao sugerido). O que nenhuma opção atende para um
//! campo obrigatório para o envio antes de sair.

use std::collections::HashMap;

use acervo_indexers::{SitePart, SiteRequest, SiteResponse};
use acervo_parser::remove_accents;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{Catalog, SearchError};

/// O indexador do Amigos no catálogo.
pub(crate) const INDEXER: &str = "amigosshare";

/// `launches.site_id` enquanto o `POST /torrents` está no ar: se o acervo cai
/// ali, o envio pode ter chegado ao site, e a marca impede outro por cima.
pub(crate) const PENDING_SITE_ID: &str = "pendente";

const UPLOAD_PAGE: &str = "/torrents/upload";

// ------------------------------------------------------------ props da página

/// O que o formulário pede para um tipo de torrent (`props.profiles`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Profile {
    pub(crate) label: String,
    /// O campo "legenda" existe neste tipo.
    #[serde(default)]
    subtitle: bool,
    /// Os ids de áudio com que a legenda é pedida (Dual e Legendado).
    #[serde(rename = "subtitleWhen", default)]
    subtitle_when: Vec<u64>,
    #[serde(rename = "coverRequired", default)]
    cover_required: bool,
    #[serde(rename = "screenshotsMin", default)]
    pub(crate) screenshots_min: usize,
    #[serde(rename = "screenshotsMax", default)]
    pub(crate) screenshots_max: usize,
    #[serde(default)]
    attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, Deserialize)]
struct Attribute {
    #[serde(rename = "type")]
    kind: String,
    label: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    multiple: bool,
    options: Vec<AttributeOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct AttributeOption {
    id: u64,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Page {
    props: Props,
}

#[derive(Debug, Deserialize)]
struct Props {
    profiles: HashMap<String, Profile>,
    #[serde(default)]
    categories: Vec<CategoryRef>,
}

#[derive(Debug, Deserialize)]
struct CategoryRef {
    id: u64,
    slug: String,
}

/// O formulário de um tipo: o perfil e a categoria que ele grava.
#[derive(Debug, Clone)]
pub(crate) struct Form {
    pub(crate) profile: Profile,
    pub(crate) category_id: u64,
}

/// As props do Inertia na página do formulário (`<script data-page>`), para o
/// tipo `slug` (`filmes`, `series`).
pub(crate) fn form_from_page(html: &str, slug: &str) -> Result<Form> {
    let start = html
        .find("data-page")
        .context("a página de envio não trouxe as props do formulário")?;
    let open = start
        + html[start..]
            .find('>')
            .context("a página de envio está cortada")?
        + 1;
    let close = open
        + html[open..]
            .find("</script>")
            .context("a página de envio está cortada")?;
    let page: Page = serde_json::from_str(html[open..close].trim())
        .context("as props do formulário mudaram de formato")?;
    let mut props = page.props;
    let profile = props
        .profiles
        .remove(slug)
        .with_context(|| format!("o formulário não tem mais o tipo `{slug}`"))?;
    let category_id = props
        .categories
        .iter()
        .find(|c| c.slug == slug)
        .map(|c| c.id)
        .with_context(|| format!("o site não tem mais a categoria `{slug}`"))?;
    Ok(Form {
        profile,
        category_id,
    })
}

// ------------------------------------------------------------ respostas

/// `/torrents/lookup`: o título pelo IMDB.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Lookup {
    #[serde(rename = "type")]
    kind: String,
    title: String,
    year: Option<i64>,
    #[serde(default)]
    genres: Vec<String>,
    language: Option<String>,
    cover: Option<Cover>,
}

#[derive(Debug, Clone, Deserialize)]
struct Cover {
    path: String,
}

/// `/torrents/mediainfo`: o que o site lê do relatório.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MediaRead {
    width: Option<u32>,
    height: Option<u32>,
    subtitle: Option<String>,
    #[serde(default)]
    attributes: HashMap<String, Vec<String>>,
    #[serde(default)]
    report: String,
}

/// `/torrents/screenshots`: a captura já hospedada no site.
#[derive(Debug, Clone, Deserialize)]
struct Hosted {
    path: String,
    url: String,
}

/// Uma captura hospedada no site: o caminho que vai em `screenshot_paths` e a
/// URL absoluta que a descrição usa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shot {
    pub(crate) path: String,
    pub(crate) url: String,
}

// ------------------------------------------------------------ escolha

/// A comparação do formulário: decomposto sem marcas, minúsculo, sem espaço
/// nas pontas.
fn fold(name: &str) -> String {
    remove_accents(name).to_lowercase().trim().to_owned()
}

/// Uma lista do formulário e o que foi escolhido nela, para a prévia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Chosen {
    pub(crate) campo: String,
    pub(crate) valor: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) ids: Vec<u64>,
    pub(crate) shown: Vec<Chosen>,
    /// Campo obrigatório sem opção que atenda: o envio não sai.
    pub(crate) missed: Vec<String>,
}

/// Escolhe a opção de cada lista como o formulário faz: gênero e idioma pelo
/// IMDB, o resto pelo `MediaInfo`, o primeiro nome sugerido que bate com uma
/// opção. `dublado` é a regra 3.9: Dual sem legenda PT vai como Dublado.
pub(crate) fn choose(
    profile: &Profile,
    media: &MediaRead,
    lookup: &Lookup,
    dublado: bool,
) -> Choice {
    let mut ids = Vec::new();
    let mut shown = Vec::new();
    let mut missed = Vec::new();
    for attribute in &profile.attributes {
        let suggested: Vec<String> = match attribute.kind.as_str() {
            "genre" => lookup.genres.clone(),
            "language" => lookup
                .language
                .iter()
                .cloned()
                .chain(
                    media
                        .attributes
                        .get("language")
                        .cloned()
                        .unwrap_or_default(),
                )
                .collect(),
            "audio_channels" if dublado => vec!["Dublado".to_owned()],
            kind => media.attributes.get(kind).cloned().unwrap_or_default(),
        };
        let find = |name: &String| {
            attribute
                .options
                .iter()
                .find(|o| fold(&o.name) == fold(name))
        };
        let picked: Vec<&AttributeOption> = if attribute.multiple {
            let mut all: Vec<&AttributeOption> = Vec::new();
            for option in suggested.iter().filter_map(find) {
                if !all.iter().any(|o| o.id == option.id) {
                    all.push(option);
                }
            }
            all
        } else {
            suggested.iter().find_map(find).into_iter().collect()
        };
        if picked.is_empty() {
            if attribute.required {
                missed.push(match suggested.first() {
                    Some(name) => format!(
                        "{} (o site sugeriu \"{name}\", que não é opção)",
                        attribute.label
                    ),
                    None => format!("{} (sem sugestão do site)", attribute.label),
                });
            }
            continue;
        }
        ids.extend(picked.iter().map(|o| o.id));
        shown.push(Chosen {
            campo: attribute.label.clone(),
            valor: picked
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    Choice { ids, shown, missed }
}

/// O campo "legenda" (`embedded`, `separate`, `none`), só quando o tipo o tem
/// e o áudio escolhido o pede. O `MediaInfo` só vê a embutida: com arquivo de
/// legenda no pacote e nada embutido, é `separate`.
fn subtitle(profile: &Profile, ids: &[u64], media: &MediaRead, has_file: bool) -> Option<String> {
    if !profile.subtitle || !ids.iter().any(|id| profile.subtitle_when.contains(id)) {
        return None;
    }
    let read = media.subtitle.as_deref().unwrap_or("none");
    Some(
        if read == "none" && has_file {
            "separate"
        } else {
            read
        }
        .to_owned(),
    )
}

// ------------------------------------------------------------ rascunho

/// O que vai ao site, montado antes de qualquer upload: é a prévia que o
/// usuário confirma e, refeito na hora, o envio.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Draft {
    pub(crate) tipo: String,
    #[serde(skip)]
    pub(crate) category_id: u64,
    pub(crate) nome: String,
    pub(crate) ano: String,
    pub(crate) imdb: String,
    pub(crate) largura: Option<u32>,
    pub(crate) altura: Option<u32>,
    #[serde(skip)]
    pub(crate) mediainfo: String,
    #[serde(skip)]
    pub(crate) description: String,
    pub(crate) capa: bool,
    #[serde(skip)]
    cover_path: Option<String>,
    #[serde(skip)]
    attribute_ids: Vec<u64>,
    pub(crate) campos: Vec<Chosen>,
    pub(crate) legenda: Option<String>,
    pub(crate) capturas: usize,
    /// O que impede o envio.
    pub(crate) faltando: Vec<String>,
}

/// O que o pacote dá ao rascunho.
#[derive(Debug, Clone)]
pub(crate) struct Package {
    /// `filmes` ou `series`.
    pub(crate) slug: &'static str,
    pub(crate) name: String,
    pub(crate) year: String,
    pub(crate) imdb_id: String,
    pub(crate) mediainfo: String,
    pub(crate) description: String,
    pub(crate) dublado: bool,
    pub(crate) has_subtitle_file: bool,
    pub(crate) screenshots: usize,
}

/// O código `tt…` (5 dígitos ou mais) num texto, como a URL do IMDB.
fn imdb_id(text: &str) -> Option<&str> {
    text.match_indices("tt").find_map(|(at, _)| {
        let digits = text[at + 2..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        (digits >= 5).then(|| &text[at..at + 2 + digits])
    })
}

/// O pacote como o envio o lê: o tipo (`report.tipo`, ou pelo nome do release
/// nos pacotes de antes dele), os campos de texto e a regra 3.9 do laudo.
pub(crate) fn package(
    launch: &acervo_store::Launch,
    has_subtitle_file: bool,
    screenshots: usize,
) -> Result<Package> {
    let field = |name: &str| {
        launch
            .fields
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let release = field("release");
    let series = match launch.report.get("tipo").and_then(Value::as_str) {
        Some(kind) => kind != "filme",
        None => acervo_parser::parse_episode_title(&release).is_some(),
    };
    let imdb_id = imdb_id(&field("imdb"))
        .map(str::to_owned)
        .context("o pacote não tem o código do IMDB: o Amigos pede a busca por ele")?;
    let mediainfo = field("mediainfo");
    if mediainfo.is_empty() {
        bail!("o pacote não tem o relatório do MediaInfo: prepare de novo");
    }
    let dublado = launch
        .report
        .get("checks")
        .and_then(Value::as_array)
        .is_some_and(|checks| {
            checks.iter().any(|c| {
                c.get("rule").and_then(Value::as_str) == Some("3.9")
                    && c.get("status").and_then(Value::as_str) == Some("warn")
            })
        });
    Ok(Package {
        slug: if series { "series" } else { "filmes" },
        name: field("titulo"),
        year: field("ano"),
        imdb_id,
        mediainfo,
        description: field("descricao"),
        dublado,
        has_subtitle_file,
        screenshots,
    })
}

/// Junta pacote, formulário e as duas leituras do site no rascunho.
pub(crate) fn draft_from(
    package: &Package,
    form: &Form,
    media: &MediaRead,
    lookup: &Lookup,
) -> Draft {
    let profile = &form.profile;
    let choice = choose(profile, media, lookup, package.dublado);
    let mut missed = choice.missed.clone();
    let want_movie = package.slug == "filmes";
    if want_movie != (lookup.kind == "movie") {
        missed.push(format!(
            "o IMDB {} diz que a obra é {}, e o pacote é {}",
            package.imdb_id,
            if lookup.kind == "movie" {
                "filme"
            } else {
                "série"
            },
            if want_movie { "filme" } else { "série" }
        ));
    }
    let cover_path = lookup.cover.as_ref().map(|c| c.path.clone());
    if profile.cover_required && cover_path.is_none() {
        missed.push("capa (o IMDB não trouxe pôster)".to_owned());
    }
    let screenshots = package.screenshots.min(profile.screenshots_max.max(1));
    if screenshots < profile.screenshots_min {
        missed.push(format!(
            "capturas: o site pede ao menos {}, o pacote tem {screenshots}",
            profile.screenshots_min
        ));
    }
    let name = if package.name.is_empty() {
        lookup.title.clone()
    } else {
        package.name.clone()
    };
    let year = if package.year.is_empty() {
        lookup.year.map(|y| y.to_string()).unwrap_or_default()
    } else {
        package.year.clone()
    };
    Draft {
        tipo: profile.label.clone(),
        category_id: form.category_id,
        nome: name,
        ano: year,
        imdb: package.imdb_id.clone(),
        largura: media.width,
        altura: media.height,
        mediainfo: if media.report.is_empty() {
            package.mediainfo.clone()
        } else {
            media.report.clone()
        },
        description: package.description.clone(),
        capa: cover_path.is_some(),
        cover_path,
        legenda: subtitle(profile, &choice.ids, media, package.has_subtitle_file),
        attribute_ids: choice.ids,
        campos: choice.shown,
        capturas: screenshots,
        faltando: missed,
    }
}

/// O corpo do `POST /torrents`, como o Inertia o monta (`forceFormData`):
/// lista vira `campo[i]`, booleano `1`/`0`, ausente vira texto vazio.
pub(crate) fn parts(
    draft: &Draft,
    torrent: Vec<u8>,
    torrent_name: &str,
    screenshots: &[String],
) -> Vec<SitePart> {
    let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    let mut parts = vec![
        SitePart::file("torrent", torrent_name, "application/x-bittorrent", torrent),
        SitePart::text("name", &draft.nome),
        SitePart::text("year", &draft.ano),
        SitePart::text("anonymous", "0"),
        SitePart::text("cover", ""),
        SitePart::text("cover_path", draft.cover_path.clone().unwrap_or_default()),
        SitePart::text("description", &draft.description),
        SitePart::text("mediainfo", &draft.mediainfo),
        SitePart::text("category_id", draft.category_id.to_string()),
        SitePart::text("imdb_id", &draft.imdb),
        SitePart::text("width", number(draft.largura)),
        SitePart::text("height", number(draft.altura)),
        SitePart::text("trailer_url", ""),
        SitePart::text("subtitle", draft.legenda.clone().unwrap_or_default()),
        SitePart::text("platform", ""),
        SitePart::text("version", ""),
        SitePart::text("store_url", ""),
    ];
    for (i, path) in screenshots.iter().enumerate() {
        parts.push(SitePart::text(format!("screenshot_paths[{i}]"), path));
    }
    for (i, id) in draft.attribute_ids.iter().enumerate() {
        parts.push(SitePart::text(
            format!("attribute_ids[{i}]"),
            id.to_string(),
        ));
    }
    parts
}

/// O id em `…/torrents/<id>`, com ou sem `/` final e query.
fn created_id(location: &str) -> Option<&str> {
    let path = location.split(['?', '#']).next().unwrap_or_default();
    let tail = path.trim_end_matches('/').rsplit_once("/torrents/")?.1;
    (!tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())).then_some(tail)
}

/// O corpo do envio com as capturas já hospedadas no site: os caminhos vão
/// em `screenshot_paths`, e as URLs entram na descrição no lugar das de fora.
pub(crate) fn upload_parts(
    draft: &Draft,
    torrent: Vec<u8>,
    torrent_name: &str,
    shots: &[Shot],
) -> Vec<SitePart> {
    let urls: Vec<String> = shots.iter().map(|s| s.url.clone()).collect();
    let paths: Vec<String> = shots.iter().map(|s| s.path.clone()).collect();
    let draft = Draft {
        description: site_description(&draft.description, &urls),
        ..draft.clone()
    };
    parts(&draft, torrent, torrent_name, &paths)
}

/// Por que o envio não deu o id do torrent criado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// O site respondeu que não aceitou (validação, ou o formulário de volta):
    /// com certeza nada foi criado. `false` é resposta que não dá para ler, ou
    /// nenhuma resposta: o torrent pode existir no site.
    pub(crate) certain: bool,
    pub(crate) message: String,
}

impl Refusal {
    fn certain(message: impl Into<String>) -> Self {
        Self {
            certain: true,
            message: message.into(),
        }
    }

    fn unknown(message: impl Into<String>) -> Self {
        Self {
            certain: false,
            message: message.into(),
        }
    }
}

/// O id do torrent criado, pelo redirect que o envio devolve; ou o motivo da
/// recusa, como o site o escreveu.
pub(crate) fn created(response: &SiteResponse) -> Result<String, Refusal> {
    if (300..400).contains(&response.status) {
        let location = response.location.as_deref().unwrap_or_default();
        if let Some(id) = created_id(location) {
            return Ok(id.to_owned());
        }
        return Err(if location.contains(UPLOAD_PAGE) {
            Refusal::certain(
                "o site devolveu o formulário sem dizer o motivo: confira o envio no site",
            )
        } else {
            Refusal::unknown(format!(
                "o site redirecionou para {location}, e não para o torrent criado"
            ))
        });
    }
    if response.status == 422 {
        let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        let mut reasons: Vec<String> = body
            .get("errors")
            .and_then(Value::as_object)
            .map(|errors| {
                errors
                    .values()
                    .filter_map(Value::as_array)
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if reasons.is_empty()
            && let Some(message) = body.get("message").and_then(Value::as_str)
        {
            reasons.push(message.to_owned());
        }
        return Err(Refusal::certain(if reasons.is_empty() {
            "o site recusou o envio sem dizer o motivo".to_owned()
        } else {
            format!("o site recusou o envio: {}", reasons.join("; "))
        }));
    }
    Err(Refusal::unknown(format!(
        "resposta inesperada do site ao envio (HTTP {})",
        response.status
    )))
}

// ------------------------------------------------------------ com o site

async fn request(catalog: &Catalog, request: SiteRequest) -> Result<SiteResponse> {
    catalog
        .site_request(INDEXER, &request)
        .await
        .map_err(|error| match error {
            SearchError::NoSuchIndexer => {
                anyhow!("o indexador do Amigos não está configurado no acervo")
            }
            other => anyhow!(other),
        })
}

/// Resposta 2xx em JSON, ou o motivo que o site deu.
fn ok_json<T: serde::de::DeserializeOwned>(response: &SiteResponse, what: &str) -> Result<T> {
    if !(200..300).contains(&response.status) {
        let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        let reason = body
            .get("errors")
            .and_then(Value::as_object)
            .and_then(|e| {
                e.values()
                    .find_map(|v| v.get(0)?.as_str().map(str::to_owned))
            })
            .or_else(|| {
                body.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("HTTP {}", response.status));
        bail!("{what}: {reason}");
    }
    serde_json::from_slice(&response.body).with_context(|| format!("{what}: resposta ilegível"))
}

/// O rascunho do envio, com as três leituras do site (página, `MediaInfo`,
/// IMDB). Nada é publicado.
pub(crate) async fn draft(catalog: &Catalog, package: &Package) -> Result<Draft> {
    let page = request(catalog, SiteRequest::get(UPLOAD_PAGE)).await?;
    if page.status != 200 {
        bail!("a página de envio do Amigos respondeu HTTP {}", page.status);
    }
    let form = form_from_page(&page.text(), package.slug)?;
    let media: MediaRead = ok_json(
        &request(
            catalog,
            SiteRequest::json(
                "/torrents/mediainfo",
                json!({ "mediainfo": package.mediainfo }).to_string(),
            ),
        )
        .await?,
        "o site não leu o MediaInfo",
    )?;
    let lookup: Lookup = ok_json(
        &request(
            catalog,
            SiteRequest::json(
                "/torrents/lookup",
                json!({ "imdb_id": package.imdb_id }).to_string(),
            ),
        )
        .await?,
        "a busca no IMDB do site falhou",
    )?;
    Ok(draft_from(package, &form, &media, &lookup))
}

/// Hospeda uma captura no site.
pub(crate) async fn host_screenshot(
    catalog: &Catalog,
    image: Vec<u8>,
    file_name: &str,
    mime: &str,
) -> Result<Shot> {
    let response = request(
        catalog,
        SiteRequest::multipart(
            "/torrents/screenshots",
            vec![SitePart::file("image", file_name, mime, image)],
        ),
    )
    .await?;
    let hosted: Hosted = ok_json(&response, "o site recusou a captura")?;
    // O site devolve `/storage/...`: absoluto pela URL que foi pedida.
    let url = url::Url::parse(&response.url)
        .and_then(|base| base.join(&hosted.url))
        .context("o site devolveu a captura com uma URL ilegível")?;
    Ok(Shot {
        path: hosted.path,
        url: url.to_string(),
    })
}

/// A descrição como o Amigos aceita: só imagem hospedada nele ("a descrição
/// só aceita imagens enviadas pelo site"). Sai toda `[img]` de fora (as
/// capturas do `ImgBB`), e as capturas do site entram sob "Capturas", ou no
/// fim, se a descrição não tem a seção.
pub(crate) fn site_description(description: &str, images: &[String]) -> String {
    const HEADER: &str = "[b]Capturas[/b]";
    let mut lines: Vec<String> = Vec::new();
    for line in description.lines() {
        let stripped = strip_images(line);
        if stripped.trim().is_empty() && stripped.len() != line.len() {
            continue;
        }
        lines.push(stripped);
    }
    let shots: Vec<String> = images
        .iter()
        .map(|url| format!("[img]{url}[/img]"))
        .collect();
    match lines.iter().position(|l| l.trim() == HEADER) {
        Some(at) if !shots.is_empty() => {
            for (offset, shot) in shots.into_iter().enumerate() {
                lines.insert(at + 1 + offset, shot);
            }
        }
        Some(at) => {
            lines.remove(at);
        }
        None if !shots.is_empty() => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.push(HEADER.to_owned());
            lines.extend(shots);
        }
        None => {}
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// A linha sem nenhum `[img]…[/img]` (a tag em qualquer caixa).
fn strip_images(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let mut out = String::new();
    let mut at = 0;
    while let Some(open) = lower[at..].find("[img") {
        let open = at + open;
        let Some(close) = lower[open..].find("[/img]") else {
            break;
        };
        out.push_str(&line[at..open]);
        at = open + close + "[/img]".len();
    }
    out.push_str(&line[at..]);
    out
}

/// O `POST /torrents`. `Ok` com o id do torrent criado. Sem resposta (rede,
/// sessão, espera do indexador) a recusa não é certa: a requisição pode ter
/// chegado.
pub(crate) async fn publish(catalog: &Catalog, parts: Vec<SitePart>) -> Result<String, Refusal> {
    let response = request(catalog, SiteRequest::multipart("/torrents", parts))
        .await
        .map_err(|error| Refusal::unknown(format!("{error:#}")))?;
    created(&response)
}

/// O `.torrent` do site para o torrent `id` (o site troca o hash do enviado).
pub(crate) async fn site_torrent(catalog: &Catalog, id: &str) -> Result<Vec<u8>> {
    let response = request(
        catalog,
        SiteRequest::get(format!("/torrents/{id}/download")),
    )
    .await?;
    if response.status != 200 || response.body.first() != Some(&b'd') {
        bail!(
            "o site não entregou o .torrent do envio (HTTP {}); pode estar aguardando liberação",
            response.status
        );
    }
    Ok(response.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><body><div id="app"></div><script data-page="app" type="application/json">{"component":"torrents/create","props":{"categories":[{"id":3,"slug":"series"},{"id":4,"slug":"filmes"}],"profiles":{"filmes":{"key":"filmes","label":"Filme","subtitle":true,"subtitleWhen":[199,198],"coverRequired":true,"screenshotsMin":2,"screenshotsMax":6,"attributes":[
        {"type":"genre","label":"Gêneros","required":true,"multiple":true,"options":[{"id":225,"name":"Animação"},{"id":206,"name":"Aventura"},{"id":207,"name":"Comédia"}]},
        {"type":"language","label":"Idioma original","required":true,"multiple":false,"options":[{"id":1,"name":"Inglês"},{"id":8,"name":"Português"}]},
        {"type":"resolution","label":"Qualidade","required":true,"multiple":false,"options":[{"id":59,"name":"WEB-DL"},{"id":49,"name":"Blu-Ray"}]},
        {"type":"audio_channels","label":"Áudio","required":true,"multiple":false,"options":[{"id":199,"name":"Dual-Audio"},{"id":200,"name":"Dublado"},{"id":198,"name":"Legendado"}]},
        {"type":"video_codec","label":"Codec de vídeo","required":true,"multiple":false,"options":[{"id":184,"name":"H264"},{"id":195,"name":"AVC"}]}
    ]},"series":{"label":"Série","attributes":[]}}},"url":"/torrents/upload"}</script></body></html>"#;

    fn media() -> MediaRead {
        serde_json::from_value(json!({
            "width": 1920, "height": 1080, "subtitle": "none",
            "report": "General\n...",
            "attributes": {
                "audio_channels": ["Dual-Audio"],
                "resolution": ["WEB-DL"],
                "video_codec": ["X264", "AVC"],
                "language": ["Inglês"]
            }
        }))
        .unwrap()
    }

    fn lookup() -> Lookup {
        serde_json::from_value(json!({
            "type": "movie", "title": "Toy Story: Um Mundo de Aventuras (Toy Story)",
            "year": 1995, "genres": ["Animação", "Aventura", "Comedia", "Família"],
            "language": "Inglês",
            "cover": {"path": "torrent-images/9e/x.webp", "url": "/storage/x.webp"}
        }))
        .unwrap()
    }

    fn package() -> Package {
        Package {
            slug: "filmes",
            name: "Toy Story: Um Mundo de Aventuras (Toy Story)".into(),
            year: "1995".into(),
            imdb_id: "tt0114709".into(),
            mediainfo: "General\n...".into(),
            description: "[b]Sinopse[/b]".into(),
            dublado: false,
            has_subtitle_file: false,
            screenshots: 4,
        }
    }

    #[test]
    fn descricao_so_com_imagens_do_site() {
        let description = "[b]Filme[/b]\n\n[b]Capturas[/b]\n[img]https://i.ibb.co/a/1.png[/img]\n[IMG]https://i.ibb.co/b/2.png[/IMG]\n";
        let images = vec![
            "https://site.invalid/storage/s/1.webp".to_owned(),
            "https://site.invalid/storage/s/2.webp".to_owned(),
        ];
        assert_eq!(
            site_description(description, &images),
            "[b]Filme[/b]\n\n[b]Capturas[/b]\n[img]https://site.invalid/storage/s/1.webp[/img]\n[img]https://site.invalid/storage/s/2.webp[/img]"
        );
        // Sem a seção, as capturas vão ao fim; imagem no meio do texto sai e o
        // texto fica.
        assert_eq!(
            site_description("Texto [img]https://x/y.png[/img] fim\n\n", &images[..1]),
            "Texto  fim\n\n[b]Capturas[/b]\n[img]https://site.invalid/storage/s/1.webp[/img]"
        );
        // Sem capturas do site, a seção vazia sai.
        assert_eq!(
            site_description("A\n[b]Capturas[/b]\n[img]https://x/1.png[/img]", &[]),
            "A"
        );
    }

    #[test]
    fn codigo_do_imdb_na_url() {
        assert_eq!(
            imdb_id("https://www.imdb.com/title/tt0114709/"),
            Some("tt0114709")
        );
        assert_eq!(imdb_id("https://attt.x/tt12/tt1234567"), Some("tt1234567"));
        assert_eq!(imdb_id("sem código"), None);
    }

    #[test]
    fn props_da_pagina_dao_o_perfil_e_a_categoria() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        assert_eq!(form.category_id, 4);
        assert_eq!(form.profile.label, "Filme");
        assert_eq!(form.profile.screenshots_min, 2);
        assert!(form_from_page(PAGE, "jogos").is_err());
        assert!(form_from_page("<html></html>", "filmes").is_err());
    }

    #[test]
    fn escolhe_como_o_formulario_pelo_nome_sem_acento() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        let choice = choose(&form.profile, &media(), &lookup(), false);
        // "Comedia" sem acento casa com "Comédia"; "Família" não é opção.
        assert_eq!(choice.ids, vec![225, 206, 207, 1, 59, 199, 195]);
        assert!(choice.missed.is_empty(), "{:?}", choice.missed);
        assert_eq!(
            choice.shown[0],
            Chosen {
                campo: "Gêneros".into(),
                valor: "Animação, Aventura, Comédia".into()
            }
        );
    }

    #[test]
    fn regra_3_9_troca_dual_por_dublado() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        let choice = choose(&form.profile, &media(), &lookup(), true);
        assert!(choice.ids.contains(&200));
        assert!(!choice.ids.contains(&199));
    }

    #[test]
    fn obrigatorio_sem_opcao_impede_o_envio() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        let mut read = media();
        read.attributes
            .insert("resolution".into(), vec!["WEBRip".into()]);
        read.attributes.remove("video_codec");
        let choice = choose(&form.profile, &read, &lookup(), false);
        assert_eq!(choice.missed.len(), 2, "{:?}", choice.missed);
        assert!(choice.missed[0].contains("WEBRip"));
        assert!(choice.missed[1].contains("sem sugestão"));
    }

    #[test]
    fn rascunho_com_legenda_capa_e_capturas() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        let draft = draft_from(&package(), &form, &media(), &lookup());
        assert!(draft.faltando.is_empty(), "{:?}", draft.faltando);
        assert_eq!(draft.legenda.as_deref(), Some("none"));
        assert!(draft.capa);
        assert_eq!(draft.capturas, 4);
        let mut separate = package();
        separate.has_subtitle_file = true;
        assert_eq!(
            draft_from(&separate, &form, &media(), &lookup())
                .legenda
                .as_deref(),
            Some("separate")
        );
    }

    #[test]
    fn rascunho_aponta_tipo_errado_e_falta_de_capturas() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        let mut series = lookup();
        series.kind = "series".into();
        let mut few = package();
        few.screenshots = 1;
        let draft = draft_from(&few, &form, &media(), &series);
        assert_eq!(draft.faltando.len(), 2, "{:?}", draft.faltando);
        assert!(draft.faltando[0].contains("diz que a obra é série"));
        assert!(draft.faltando[1].contains("ao menos 2"));
    }

    #[test]
    fn corpo_do_envio_como_o_inertia_monta() {
        let form = form_from_page(PAGE, "filmes").unwrap();
        let draft = draft_from(&package(), &form, &media(), &lookup());
        let parts = parts(
            &draft,
            b"d4:infoe".to_vec(),
            "x.torrent",
            &["a.webp".into(), "b.webp".into()],
        );
        let text = |name: &str| {
            parts
                .iter()
                .find(|p| p.name == name)
                .map(|p| match &p.value {
                    acervo_indexers::SitePartValue::Text(t) => t.clone(),
                    acervo_indexers::SitePartValue::File { file_name, .. } => {
                        format!("arquivo {file_name}")
                    }
                })
        };
        assert_eq!(text("torrent").as_deref(), Some("arquivo x.torrent"));
        assert_eq!(text("category_id").as_deref(), Some("4"));
        assert_eq!(
            text("cover_path").as_deref(),
            Some("torrent-images/9e/x.webp")
        );
        assert_eq!(text("screenshot_paths[1]").as_deref(), Some("b.webp"));
        assert_eq!(text("attribute_ids[0]").as_deref(), Some("225"));
        assert_eq!(text("subtitle").as_deref(), Some("none"));
        assert_eq!(text("anonymous").as_deref(), Some("0"));
    }

    #[test]
    fn resposta_do_envio_da_o_id_ou_o_motivo() {
        let response = |status, location: Option<&str>, body: &str| SiteResponse {
            url: "https://site.invalid/torrents".to_owned(),
            status,
            location: location.map(str::to_owned),
            body: body.as_bytes().to_vec(),
        };
        assert_eq!(
            created(&response(
                302,
                Some("https://amigos-share.club/torrents/72560"),
                ""
            )),
            Ok("72560".into())
        );
        assert_eq!(
            created(&response(303, Some("/torrents/9/"), "")),
            Ok("9".into())
        );
        let back = created(&response(302, Some("/torrents/upload"), "")).unwrap_err();
        assert!(back.certain && back.message.contains("formulário"));
        let refused = created(&response(422, None, r#"{"message":"x","errors":{"name":["Nome fora do padrão"],"cover":["Capa obrigatória"]}}"#)).unwrap_err();
        assert!(refused.certain);
        assert!(
            refused.message.contains("Nome fora do padrão")
                && refused.message.contains("Capa obrigatória"),
            "{refused:?}"
        );
        // Sem dar para ler a resposta, o torrent pode ter sido criado.
        let unknown = created(&response(500, None, "")).unwrap_err();
        assert!(!unknown.certain && unknown.message.contains("HTTP 500"));
        assert!(
            !created(&response(302, Some("/dashboard"), ""))
                .unwrap_err()
                .certain
        );
    }
}
