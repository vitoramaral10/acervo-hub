//! Eventos: o que aconteceu com cada filme vai para o histórico e, se o
//! evento estiver ligado, para as notificações (Gotify).
//!
//! Notificar é melhor esforço: servidor fora do ar vira aviso no log, nunca
//! erro de quem gerou o evento.

use std::time::Duration;

use acervo_parser::Quality;
use acervo_store::{NewHistory, Store};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::shadow::now_rfc3339;

/// Onde a configuração das notificações fica.
pub const NOTIFY_KEY: &str = "notificacoes.gotify";

/// Tipos de evento, como o histórico os grava.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Grabbed,
    Imported,
    Upgraded,
    Failed,
    FileDeleted,
    MovieAdded,
    MovieDeleted,
    /// Download tirado da fila sem importar.
    Ignored,
}

impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grabbed => "grabbed",
            Self::Imported => "imported",
            Self::Upgraded => "upgraded",
            Self::Failed => "failed",
            Self::FileDeleted => "file_deleted",
            Self::MovieAdded => "movie_added",
            Self::MovieDeleted => "movie_deleted",
            Self::Ignored => "ignored",
        }
    }

    const fn verb(self) -> &'static str {
        match self {
            Self::Grabbed => "Pegou",
            Self::Imported => "Importou",
            Self::Upgraded => "Trocou por versão melhor",
            Self::Failed => "Download falhou",
            Self::FileDeleted => "Arquivo apagado",
            Self::MovieAdded => "Filme adicionado",
            Self::MovieDeleted => "Filme removido",
            Self::Ignored => "Download descartado",
        }
    }
}

/// Um evento a registrar.
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: Kind,
    pub movie_id: Option<i64>,
    /// "Título (Ano)".
    pub movie: String,
    pub source_title: Option<String>,
    pub quality: Option<Quality>,
    pub indexer: Option<String>,
    pub download_id: Option<String>,
    /// Detalhe livre: motivo da falha, destino do arquivo...
    pub message: Option<String>,
    pub poster: Option<String>,
}

impl Event {
    #[must_use]
    pub fn new(kind: Kind, movie_id: Option<i64>, movie: impl Into<String>) -> Self {
        Self {
            kind,
            movie_id,
            movie: movie.into(),
            source_title: None,
            quality: None,
            indexer: None,
            download_id: None,
            message: None,
            poster: None,
        }
    }
}

/// Que eventos notificar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // Uma chave por evento, como a tela mostra.
pub struct NotifyOn {
    pub pegou: bool,
    pub importou: bool,
    pub atualizou: bool,
    pub falhou: bool,
    pub removido: bool,
}

impl Default for NotifyOn {
    fn default() -> Self {
        Self {
            pegou: true,
            importou: true,
            atualizou: true,
            falhou: true,
            removido: false,
        }
    }
}

/// A configuração do Gotify guardada no banco. O token nunca volta à tela.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gotify {
    pub servidor: String,
    pub token: String,
    #[serde(default = "default_priority")]
    pub prioridade: u8,
    #[serde(default)]
    pub eventos: NotifyOn,
    #[serde(default = "yes")]
    pub ligado: bool,
}

const fn default_priority() -> u8 {
    5
}

const fn yes() -> bool {
    true
}

impl Gotify {
    const fn wants(&self, kind: Kind) -> bool {
        self.ligado
            && match kind {
                Kind::Grabbed => self.eventos.pegou,
                Kind::Imported => self.eventos.importou,
                Kind::Upgraded => self.eventos.atualizou,
                Kind::Failed => self.eventos.falhou,
                Kind::MovieDeleted | Kind::FileDeleted => self.eventos.removido,
                Kind::MovieAdded | Kind::Ignored => false,
            }
    }

    /// Manda uma mensagem.
    ///
    /// # Errors
    ///
    /// Servidor inalcançável ou token recusado.
    pub async fn send(&self, title: &str, body: &str, poster: Option<&str>) -> Result<()> {
        let mut url = url::Url::parse(self.servidor.trim_end_matches('/'))?;
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("endereço do Gotify inválido"))?
            .push("message");
        let mut extras = json!({ "client::display": { "contentType": "text/markdown" } });
        if let Some(poster) = poster {
            extras["client::notification"] = json!({ "bigImageUrl": poster });
        }
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?
            .post(url)
            .header("X-Gotify-Key", &self.token)
            .json(&json!({
                "title": title,
                "message": body,
                "priority": self.prioridade,
                "extras": extras,
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("Gotify respondeu {}", response.status());
        }
        Ok(())
    }
}

/// A configuração das notificações, se houver.
///
/// # Errors
///
/// Banco inalcançável.
pub async fn gotify(store: &Store) -> Result<Option<Gotify>> {
    Ok(store
        .setting(NOTIFY_KEY)
        .await?
        .and_then(|text| serde_json::from_str(&text).ok()))
}

fn body(event: &Event) -> String {
    let mut lines = Vec::new();
    if let Some(title) = &event.source_title {
        lines.push(format!("`{title}`"));
    }
    let facts: Vec<String> = [
        event.quality.map(|q| q.name().to_owned()),
        event.indexer.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }
    if let Some(message) = &event.message {
        lines.push(message.clone());
    }
    lines.join("\n\n")
}

/// Grava o evento no histórico e notifica, se for o caso. Falha de gravação
/// vira aviso no log: o evento é consequência, não pode desfazer a ação.
pub async fn record(store: &Store, event: Event) {
    let entry = NewHistory {
        movie_id: event.movie_id,
        movie_title: event.movie.clone(),
        event: event.kind.as_str().to_owned(),
        at: now_rfc3339(),
        source_title: event.source_title.clone(),
        quality: event.quality,
        indexer: event.indexer.clone(),
        download_id: event.download_id.clone(),
        data: event
            .message
            .as_ref()
            .map_or_else(|| json!({}), |m| json!({ "mensagem": m })),
    };
    if let Err(error) = store.record_history(&entry).await {
        tracing::warn!(filme = event.movie, "histórico: {error}");
    }
    match gotify(store).await {
        Ok(Some(gotify)) if gotify.wants(event.kind) => {
            let title = format!("{}: {}", event.kind.verb(), event.movie);
            let body = body(&event);
            let poster = event.poster.clone();
            // Não prende quem gerou o evento esperando o servidor.
            tokio::spawn(async move {
                if let Err(error) = gotify.send(&title, &body, poster.as_deref()).await {
                    tracing::warn!("notificação: {error:#}");
                }
            });
        }
        Ok(_) => {}
        Err(error) => tracing::warn!("notificação: {error}"),
    }
}

/// O evento mais recente de cada filme num tipo, para a tela.
#[must_use]
pub fn label(title: &str, year: Option<u16>) -> String {
    match year {
        Some(year) => format!("{title} ({year})"),
        None => title.to_owned(),
    }
}

/// Converte a configuração da tela, sem o token (que só entra, nunca sai).
#[must_use]
pub fn public_view(gotify: Option<&Gotify>) -> Value {
    gotify.map_or_else(
        || json!(null),
        |g| {
            json!({
                "servidor": g.servidor,
                "token_definido": !g.token.is_empty(),
                "prioridade": g.prioridade,
                "eventos": g.eventos,
                "ligado": g.ligado,
            })
        },
    )
}
