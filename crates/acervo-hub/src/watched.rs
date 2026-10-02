//! Apagar assistidos: o Jellyfin diz o que cada usuário assistiu, e o filme
//! que alguém assistiu, passada a carência, sai do acervo — catálogo e
//! arquivos, sem simulação.
//!
//! A regra é por filme, somando os usuários: sai quando qualquer um assistiu,
//! a última vez que alguém assistiu foi há mais que a carência, e ninguém o
//! marcou como favorito. Na dúvida, fica: assistido sem data conhecida não
//! sai.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use acervo_clients::jellyfin::JellyfinClient;
use acervo_core::Allocated;
use acervo_store::{CatalogMovie, Store};
use anyhow::{Context, Result};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::config::Config;

/// O que um usuário fez com um filme do Jellyfin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewing {
    pub user: String,
    pub tmdb_id: Option<u32>,
    pub played: bool,
    /// `None` também quando o servidor mandou uma data ilegível.
    pub last_played: Option<OffsetDateTime>,
    pub favorite: bool,
}

/// O destino de um filme que alguém assistiu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Sai: `user` foi o último a assistir, em `at`.
    Delete { user: String, at: OffsetDateTime },
    /// Fica: alguém o marcou como favorito.
    Favorite { user: String },
    /// Fica até `until`.
    InGrace { until: OffsetDateTime },
    /// Fica: `user` assistiu, mas não se sabe quando.
    NoDate { user: String },
}

/// A regra, sem rede nem banco. Só entra quem tem id do TMDB e foi
/// assistido por alguém; o resto não é assunto da tarefa.
#[must_use]
pub fn select(
    viewings: &[Viewing],
    now: OffsetDateTime,
    grace: Duration,
) -> BTreeMap<u32, Verdict> {
    let mut by_movie: BTreeMap<u32, Vec<&Viewing>> = BTreeMap::new();
    for viewing in viewings {
        if let Some(tmdb_id) = viewing.tmdb_id {
            by_movie.entry(tmdb_id).or_default().push(viewing);
        }
    }
    by_movie
        .into_iter()
        .filter_map(|(tmdb_id, views)| {
            let played: Vec<&&Viewing> = views.iter().filter(|v| v.played).collect();
            if played.is_empty() {
                return None;
            }
            // Favorito de qualquer usuário segura, mesmo de quem não assistiu.
            if let Some(fan) = views.iter().find(|v| v.favorite) {
                return Some((
                    tmdb_id,
                    Verdict::Favorite {
                        user: fan.user.clone(),
                    },
                ));
            }
            // Uma visualização sem data pode ser a mais recente: não dá
            // para saber se a carência passou.
            if let Some(undated) = played.iter().find(|v| v.last_played.is_none()) {
                return Some((
                    tmdb_id,
                    Verdict::NoDate {
                        user: undated.user.clone(),
                    },
                ));
            }
            let (user, at) = played
                .iter()
                .filter_map(|v| v.last_played.map(|at| (&v.user, at)))
                .max_by_key(|(_, at)| *at)?;
            let verdict = if now - at > grace {
                Verdict::Delete {
                    user: user.clone(),
                    at,
                }
            } else {
                Verdict::InGrace {
                    until: at.checked_add(grace).unwrap_or(at),
                }
            };
            Some((tmdb_id, verdict))
        })
        .collect()
}

/// Um filme apagado.
#[derive(Debug, Clone, Serialize)]
pub struct Deleted {
    pub titulo: String,
    pub ano: Option<u16>,
    pub assistido_por: String,
    /// RFC 3339.
    pub assistido_em: String,
    pub tamanho: String,
}

/// Um filme assistido que ficou, e por quê.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub titulo: String,
    pub ano: Option<u16>,
    pub motivo: String,
}

/// O detalhe de uma execução.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WatchedReport {
    pub apagados: Vec<Deleted>,
    pub pulados: Vec<Skipped>,
    /// Quantos dos pulados a remoção recusou.
    pub recusados: usize,
    pub liberado: String,
    /// Falha que não desfaz nada, como a varredura do Jellyfin.
    pub aviso: Option<String>,
}

impl WatchedReport {
    /// Uma linha para a lista de tarefas, e se a execução terminou bem.
    /// Remoção recusada é erro na tela: alguém precisa olhar.
    #[must_use]
    pub fn summary(&self) -> (bool, String) {
        let deleted = self.apagados.len();
        if deleted == 0 && self.recusados == 0 {
            return (true, "nada a apagar".into());
        }
        let mut line = if deleted == 1 {
            format!("1 apagado, {} liberados", self.liberado)
        } else {
            format!("{deleted} apagados, {} liberados", self.liberado)
        };
        match self.recusados {
            0 => {}
            1 => line.push_str(", 1 recusado"),
            n => {
                use std::fmt::Write as _;
                let _ = write!(line, ", {n} recusados");
            }
        }
        (self.recusados == 0, line)
    }
}

/// Data do Jellyfin (ISO 8601 em UTC, com até sete casas de fração).
fn parse_date(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// Para o histórico e para o motivo: legível, em UTC.
fn human(at: OffsetDateTime) -> String {
    let format = time::macros::format_description!("[day]/[month]/[year] [hour]:[minute] UTC");
    at.format(&format).unwrap_or_default()
}

/// O que a pasta do filme ocupa no disco, antes de apagar. Pasta que não
/// existe não libera nada.
async fn folder_size(config: &Config, entry: &CatalogMovie) -> Allocated {
    let Ok(host) = config.path_map().to_host(Path::new(&entry.movie.path)) else {
        return Allocated::ZERO;
    };
    tokio::task::spawn_blocking(move || {
        if host.is_dir() {
            acervo_fs::measure_roots(&[host])
        } else {
            Allocated::ZERO
        }
    })
    .await
    .unwrap_or(Allocated::ZERO)
}

/// Uma execução: lê o Jellyfin, aplica a regra e remove.
///
/// # Errors
///
/// Jellyfin inalcançável ou catálogo ilegível — antes de apagar qualquer
/// coisa. Falha num filme fica no relatório; os outros seguem.
pub async fn run(
    config: &Config,
    store: &Store,
    jellyfin: &JellyfinClient,
    grace_minutes: u64,
) -> Result<WatchedReport> {
    let users = jellyfin
        .users()
        .await
        .context("listando os usuários do Jellyfin")?;
    let mut viewings = Vec::new();
    for user in &users {
        let movies = jellyfin
            .movies(&user.id)
            .await
            .with_context(|| format!("lendo os filmes de `{}` no Jellyfin", user.name))?;
        viewings.extend(movies.into_iter().map(|movie| Viewing {
            user: user.name.clone(),
            tmdb_id: movie.tmdb_id,
            played: movie.played,
            last_played: movie.last_played.as_deref().and_then(parse_date),
            favorite: movie.favorite,
        }));
    }
    let grace =
        Duration::seconds(i64::try_from(grace_minutes.saturating_mul(60)).unwrap_or(i64::MAX));
    let verdicts = select(&viewings, OffsetDateTime::now_utc(), grace);

    let catalog: HashMap<u32, CatalogMovie> = store
        .movies()
        .await?
        .into_iter()
        .map(|entry| (entry.movie.tmdb_id, entry))
        .collect();
    let mut report = WatchedReport::default();
    let mut freed = Allocated::ZERO;
    for (tmdb_id, verdict) in verdicts {
        let Some(entry) = catalog.get(&tmdb_id) else {
            continue;
        };
        let skip = |motivo: String| Skipped {
            titulo: entry.movie.title.clone(),
            ano: entry.movie.year,
            motivo,
        };
        match verdict {
            Verdict::Delete { user, at } => {
                // Medido antes: depois de apagar, não há o que medir.
                let size = folder_size(config, entry).await;
                let reason = format!("assistido por {user} em {}", human(at));
                match crate::library::remove_because(
                    config,
                    store,
                    entry.id,
                    true,
                    false,
                    Some(&reason),
                )
                .await
                {
                    Ok(()) => {
                        freed = freed + size;
                        report.apagados.push(Deleted {
                            titulo: entry.movie.title.clone(),
                            ano: entry.movie.year,
                            assistido_por: user,
                            assistido_em: at.format(&Rfc3339).unwrap_or_default(),
                            tamanho: size.to_string(),
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            filme = entry.movie.title,
                            "assistido não removido: {error:#}"
                        );
                        report.recusados += 1;
                        report
                            .pulados
                            .push(skip(format!("remoção recusada: {error:#}")));
                    }
                }
            }
            Verdict::Favorite { user } => report.pulados.push(skip(format!("favorito de {user}"))),
            Verdict::InGrace { until } => report
                .pulados
                .push(skip(format!("dentro da carência, até {}", human(until)))),
            Verdict::NoDate { user } => report
                .pulados
                .push(skip(format!("assistido por {user} sem data conhecida"))),
        }
    }
    report.liberado = freed.to_string();
    if !report.apagados.is_empty()
        && let Err(error) = jellyfin.refresh_library().await
    {
        tracing::warn!("varredura do Jellyfin: {error}");
        report.aviso = Some(format!("varredura da biblioteca não pedida: {error}"));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use acervo_store::{Movie, MovieExtras};
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn at(text: &str) -> OffsetDateTime {
        parse_date(text).unwrap()
    }

    fn view(
        user: &str,
        tmdb: Option<u32>,
        played: bool,
        when: Option<&str>,
        favorite: bool,
    ) -> Viewing {
        Viewing {
            user: user.into(),
            tmdb_id: tmdb,
            played,
            last_played: when.map(at),
            favorite,
        }
    }

    const NOW: &str = "2026-10-02T12:00:00Z";

    fn decide(viewings: &[Viewing], grace_minutes: i64) -> BTreeMap<u32, Verdict> {
        select(viewings, at(NOW), Duration::minutes(grace_minutes))
    }

    #[test]
    fn data_do_jellyfin_com_sete_casas_e_lida() {
        assert_eq!(
            parse_date("2026-09-30T21:14:08.1234567Z"),
            Some(time::macros::datetime!(2026-09-30 21:14:08.1234567 UTC))
        );
        assert_eq!(parse_date("ontem"), None);
        assert_eq!(human(at("2026-09-30T21:14:08Z")), "30/09/2026 21:14 UTC");
    }

    #[test]
    fn so_o_assistido_entra_e_sem_tmdb_fica_de_fora() {
        let verdicts = decide(
            &[
                view("vitor", Some(1), false, None, false),
                view("vitor", None, true, Some("2026-09-01T00:00:00Z"), false),
                view("vitor", Some(2), true, Some("2026-09-01T00:00:00Z"), false),
            ],
            60,
        );
        assert_eq!(
            verdicts.into_iter().collect::<Vec<_>>(),
            [(
                2,
                Verdict::Delete {
                    user: "vitor".into(),
                    at: at("2026-09-01T00:00:00Z")
                }
            )]
        );
    }

    #[test]
    fn carencia_conta_da_ultima_vez() {
        // Há 30 minutos, com 60 de carência: fica até 11:30 + 60.
        let recente = [view(
            "vitor",
            Some(1),
            true,
            Some("2026-10-02T11:30:00Z"),
            false,
        )];
        assert_eq!(
            decide(&recente, 60)[&1],
            Verdict::InGrace {
                until: at("2026-10-02T12:30:00Z")
            }
        );
        // Há 90 minutos, sai.
        let velho = [view(
            "vitor",
            Some(1),
            true,
            Some("2026-10-02T10:30:00Z"),
            false,
        )];
        assert!(matches!(decide(&velho, 60)[&1], Verdict::Delete { .. }));
        // Exatamente na carência ainda fica: é "há mais de".
        let limite = [view(
            "vitor",
            Some(1),
            true,
            Some("2026-10-02T11:00:00Z"),
            false,
        )];
        assert!(matches!(decide(&limite, 60)[&1], Verdict::InGrace { .. }));
    }

    #[test]
    fn favorito_de_outro_usuario_segura() {
        let verdicts = decide(
            &[
                view("vitor", Some(1), true, Some("2026-09-01T00:00:00Z"), false),
                view("ana", Some(1), false, None, true),
            ],
            60,
        );
        assert_eq!(verdicts[&1], Verdict::Favorite { user: "ana".into() });
    }

    #[test]
    fn assistido_sem_data_fica() {
        let verdicts = decide(
            &[
                view("vitor", Some(1), true, Some("2026-09-01T00:00:00Z"), false),
                view("ana", Some(1), true, None, false),
            ],
            60,
        );
        assert_eq!(verdicts[&1], Verdict::NoDate { user: "ana".into() });
    }

    #[test]
    fn dois_usuarios_vale_a_data_mais_recente() {
        let viewings = [
            view("vitor", Some(1), true, Some("2026-10-02T09:00:00Z"), false),
            view("ana", Some(1), true, Some("2026-10-02T11:30:00Z"), false),
        ];
        // A de 9h passou da carência; a de 11h30, não.
        assert_eq!(
            decide(&viewings, 60)[&1],
            Verdict::InGrace {
                until: at("2026-10-02T12:30:00Z")
            }
        );
        assert_eq!(
            decide(&viewings, 10)[&1],
            Verdict::Delete {
                user: "ana".into(),
                at: at("2026-10-02T11:30:00Z")
            }
        );
    }

    #[test]
    fn resumo_conta_apagados_e_recusados() {
        assert_eq!(
            WatchedReport::default().summary(),
            (true, "nada a apagar".into())
        );
        let mut report = WatchedReport {
            liberado: "1.5 GiB".into(),
            ..WatchedReport::default()
        };
        report.apagados.push(Deleted {
            titulo: "A".into(),
            ano: None,
            assistido_por: "vitor".into(),
            assistido_em: NOW.into(),
            tamanho: "1.5 GiB".into(),
        });
        assert_eq!(
            report.summary(),
            (true, "1 apagado, 1.5 GiB liberados".into())
        );
        report.recusados = 2;
        assert_eq!(
            report.summary(),
            (false, "1 apagado, 1.5 GiB liberados, 2 recusados".into())
        );
    }

    fn movie(tmdb_id: u32, title: &str, path: &str) -> Movie {
        Movie {
            tmdb_id,
            imdb_id: None,
            title: title.into(),
            original_title: None,
            original_language: None,
            year: Some(2024),
            status: None,
            minimum_availability: Some("released".into()),
            monitored: false,
            quality_profile: None,
            path: path.into(),
            added: None,
            file: None,
            runtime: 0,
            secondary_year: None,
            clean_title: None,
            alternate_titles: Vec::new(),
            available: true,
            in_cinemas: None,
            digital_release: None,
            physical_release: None,
            overview: None,
            tags: Vec::new(),
        }
    }

    fn item(name: &str, tmdb: u32, data: &serde_json::Value) -> serde_json::Value {
        json!({
            "Name": name, "Id": format!("id{tmdb}"), "Type": "Movie", "ProductionYear": 2024,
            "ProviderIds": { "Tmdb": tmdb.to_string() }, "UserData": data
        })
    }

    #[tokio::test]
    async fn apaga_o_assistido_e_poupa_o_favorito() {
        let Some(db) = acervo_store::testing::TestDb::new("assistidos").await else {
            return;
        };
        let root = std::env::temp_dir().join(format!("acervo-assistidos-{}", std::process::id()));
        let visto = root.join("movies/Visto (2024)");
        let amado = root.join("movies/Amado (2024)");
        std::fs::create_dir_all(&visto).unwrap();
        std::fs::create_dir_all(&amado).unwrap();
        std::fs::write(visto.join("visto.mkv"), vec![0_u8; 64 * 1024]).unwrap();
        std::fs::write(amado.join("amado.mkv"), b"fica").unwrap();
        let mut config = Config::default();
        config
            .library
            .paths
            .insert("/media".into(), root.display().to_string());
        config.library.root_folders = vec!["/media/movies".into()];
        let store = &db.store;
        let extras = MovieExtras::default();
        store
            .add_movie(&movie(10, "Visto", "/media/movies/Visto (2024)"), &extras)
            .await
            .unwrap();
        store
            .add_movie(&movie(20, "Amado", "/media/movies/Amado (2024)"), &extras)
            .await
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "Name": "vitor", "Id": "u1" }, { "Name": "ana", "Id": "u2" }
            ])))
            .mount(&server)
            .await;
        let played = json!({ "Played": true, "IsFavorite": false, "LastPlayedDate": "2026-09-01T20:00:00.0000000Z" });
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("userId", "u1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [item("Visto", 10, &played), item("Amado", 20, &played),
                          // Fora do catálogo: ignorado.
                          item("Outro", 30, &played)],
                "TotalRecordCount": 3
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Items"))
            .and(query_param("userId", "u2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [item("Amado", 20, &json!({ "Played": false, "IsFavorite": true }))],
                "TotalRecordCount": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Library/Refresh"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let jellyfin =
            JellyfinClient::new(&server.uri(), "chave", StdDuration::from_secs(5)).unwrap();

        let report = run(&config, store, &jellyfin, 60).await.unwrap();
        assert_eq!(report.apagados.len(), 1, "{report:?}");
        assert_eq!(report.apagados[0].titulo, "Visto");
        assert_eq!(report.apagados[0].assistido_por, "vitor");
        assert_eq!(report.apagados[0].assistido_em, "2026-09-01T20:00:00Z");
        assert_ne!(report.liberado, "0 B");
        assert_eq!(report.pulados.len(), 1);
        assert_eq!(report.pulados[0].titulo, "Amado");
        assert_eq!(report.pulados[0].motivo, "favorito de ana");
        assert!(report.summary().0);

        assert!(!visto.exists());
        assert!(amado.join("amado.mkv").exists());
        let left: Vec<_> = store
            .movies()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.movie.tmdb_id)
            .collect();
        assert_eq!(left, [20]);
        let history = store
            .history(None, Some("movie_deleted"), 10, 0)
            .await
            .unwrap();
        let message = history.events[0].data["mensagem"].as_str().unwrap();
        assert!(
            message.starts_with("assistido por vitor em 01/09/2026 20:00 UTC; pasta apagada"),
            "{message}"
        );

        // De novo: nada mais a apagar, e sem varredura (o `expect(1)` confere).
        let again = run(&config, store, &jellyfin, 60).await.unwrap();
        assert_eq!(again.summary(), (true, "nada a apagar".into()));
        std::fs::remove_dir_all(&root).unwrap();
        db.drop().await;
    }
}
