//! Sugestões de assistidos: o Jellyfin diz o que cada usuário assistiu, e o
//! filme que a regra apagaria vira sugestão na tela "Para apagar". Nada sai
//! sozinho: o usuário marca e confirma.
//!
//! A regra é por filme, somando os usuários: entra quando qualquer um
//! assistiu, a última vez que alguém assistiu foi há mais que a carência, e
//! ninguém o marcou como favorito. Na dúvida, fica de fora: assistido sem
//! data conhecida não entra. E só entra o arquivo que chegou antes de
//! assistirem: o Jellyfin lembra o assistido de um filme apagado, e o mesmo
//! filme adicionado de novo seria sugerido assim que importado.

use std::collections::BTreeMap;

use acervo_clients::jellyfin::JellyfinClient;
use acervo_store::{CatalogMovie, Store};
use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

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
    /// A regra apagaria: `user` foi o último a assistir, em `at`.
    Delete { user: String, at: OffsetDateTime },
    /// Fica: alguém o marcou como favorito.
    Favorite { user: String },
    /// Fica até `until`.
    InGrace { until: OffsetDateTime },
    /// Fica: `user` assistiu, mas não se sabe quando.
    NoDate { user: String },
}

/// A regra, sem rede nem banco. Só entra quem tem id do TMDB e foi
/// assistido por alguém; o resto não é assunto da regra.
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

/// Um filme que a regra apagaria: assistido por `user` em `at`, passada a
/// carência, sem favorito, e o arquivo de agora chegou antes de assistirem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub movie_id: i64,
    pub user: String,
    pub at: OffsetDateTime,
}

/// Os vereditos que viram sugestão, sem rede nem banco. Fica de fora o
/// filme fora do catálogo e o que não sairia: favorito, na carência, sem
/// data, ou com arquivo que chegou depois de assistirem (o Jellyfin lembra
/// o assistido de um filme apagado, e o mesmo filme adicionado de novo
/// seria sugerido assim que importado). Na ordem do catálogo.
#[must_use]
pub fn suggestions(verdicts: &BTreeMap<u32, Verdict>, catalog: &[CatalogMovie]) -> Vec<Suggestion> {
    catalog
        .iter()
        .filter_map(|entry| match verdicts.get(&entry.movie.tmdb_id)? {
            Verdict::Delete { user, at } => {
                let added = entry.movie.file.as_ref()?.date_added.as_deref();
                watched_after_added(*at, added).then(|| Suggestion {
                    movie_id: entry.id,
                    user: user.clone(),
                    at: *at,
                })
            }
            _ => None,
        })
        .collect()
}

/// Assistido depois de o arquivo de agora chegar à biblioteca? Data de
/// adição ausente ou ilegível é "não sei": não.
pub(crate) fn watched_after_added(at: OffsetDateTime, date_added: Option<&str>) -> bool {
    date_added
        .and_then(|added| OffsetDateTime::parse(added, &Rfc3339).ok())
        .is_some_and(|added| at > added)
}

/// Data do Jellyfin (ISO 8601 em UTC, com até sete casas de fração).
pub(crate) fn parse_date(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// A carência da configuração, em minutos, como duração.
pub(crate) fn grace(minutes: u64) -> Duration {
    Duration::seconds(i64::try_from(minutes.saturating_mul(60)).unwrap_or(i64::MAX))
}

/// Lê o Jellyfin e devolve os filmes do catálogo que a regra apagaria. Só
/// lê: apagar é com o usuário, pela tela "Para apagar".
///
/// # Errors
///
/// Jellyfin inalcançável ou catálogo ilegível.
pub async fn suggest(
    store: &Store,
    jellyfin: &JellyfinClient,
    grace_minutes: u64,
) -> Result<Vec<Suggestion>> {
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
    let verdicts = select(&viewings, OffsetDateTime::now_utc(), grace(grace_minutes));
    Ok(suggestions(&verdicts, &store.movies().await?))
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
    fn so_sai_o_arquivo_que_chegou_antes_de_assistirem() {
        let seen = at("2026-09-01T20:00:00Z");
        assert!(watched_after_added(seen, Some("2026-09-01T19:59:59Z")));
        assert!(!watched_after_added(seen, Some("2026-09-01T20:00:00Z")));
        assert!(!watched_after_added(seen, Some("2026-09-10T00:00:00Z")));
        assert!(!watched_after_added(seen, None));
        assert!(!watched_after_added(seen, Some("ontem")));
    }

    /// Um filme com arquivo adicionado em `added`; sem `added`, sem data de
    /// adição.
    fn movie(tmdb_id: u32, title: &str, added: Option<&str>) -> Movie {
        Movie {
            tmdb_id,
            imdb_id: None,
            title: title.into(),
            original_title: None,
            original_language: None,
            year: Some(2024),
            status: None,
            monitored: false,
            path: format!("/media/movies/{title} (2024)"),
            added: None,
            file: Some(acervo_store::MovieFile {
                relative_path: format!("{title}.mkv"),
                size: 1,
                quality: acervo_parser::parse_quality("x.1080p.WEB-DL"),
                languages: Vec::new(),
                release_group: None,
                edition: None,
                scene_name: None,
                date_added: added.map(str::to_owned),
            }),
            runtime: 0,
            secondary_year: None,
            clean_title: None,
            alternate_titles: Vec::new(),
            in_cinemas: None,
            digital_release: None,
            physical_release: None,
            overview: None,
        }
    }

    fn entry(id: i64, movie: Movie) -> CatalogMovie {
        CatalogMovie {
            id,
            movie,
            extras: MovieExtras::default(),
            priority: false,
            subtitles: Vec::new(),
        }
    }

    #[test]
    fn so_vira_sugestao_o_que_a_regra_apagaria() {
        let seen = at("2026-09-01T20:00:00Z");
        let delete = |user: &str| Verdict::Delete {
            user: user.into(),
            at: seen,
        };
        let before = Some("2026-08-01T00:00:00Z");
        let verdicts = BTreeMap::from([
            (10, delete("vitor")),
            (20, Verdict::Favorite { user: "ana".into() }),
            (40, delete("vitor")),
            (50, delete("vitor")),
            (60, delete("ana")),
            // Fora do catálogo: ignorado.
            (99, delete("vitor")),
        ]);
        let mut no_file = movie(60, "Sem Arquivo", before);
        no_file.file = None;
        let catalog = [
            entry(1, movie(10, "Visto", before)),
            entry(2, movie(20, "Amado", before)),
            // Arquivo que chegou depois de assistirem.
            entry(4, movie(40, "De Novo", Some("2026-09-10T00:00:00Z"))),
            // Sem data de adição: na dúvida, fica de fora.
            entry(5, movie(50, "Sem Data", None)),
            entry(6, no_file),
            // Ninguém assistiu.
            entry(7, movie(70, "Novo", before)),
        ];
        assert_eq!(
            suggestions(&verdicts, &catalog),
            [Suggestion {
                movie_id: 1,
                user: "vitor".into(),
                at: seen
            }]
        );
    }

    fn item(name: &str, tmdb: u32, data: &serde_json::Value) -> serde_json::Value {
        json!({
            "Name": name, "Id": format!("id{tmdb}"), "Type": "Movie", "ProductionYear": 2024,
            "ProviderIds": { "Tmdb": tmdb.to_string() }, "UserData": data
        })
    }

    #[tokio::test]
    async fn sugere_o_assistido_sem_apagar_nada() {
        let Some(db) = acervo_store::testing::TestDb::new("sugestoes").await else {
            return;
        };
        let store = &db.store;
        let extras = MovieExtras::default();
        let before = Some("2026-08-01T00:00:00Z");
        let visto = store
            .add_movie(&movie(10, "Visto", before), &extras)
            .await
            .unwrap();
        store
            .add_movie(&movie(20, "Amado", before), &extras)
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
                "Items": [item("Visto", 10, &played), item("Amado", 20, &played)],
                "TotalRecordCount": 2
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
        // Sugerir não pede varredura: nada saiu.
        Mock::given(method("POST"))
            .and(path("/Library/Refresh"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let jellyfin =
            JellyfinClient::new(&server.uri(), "chave", StdDuration::from_secs(5)).unwrap();

        let found = suggest(store, &jellyfin, 60).await.unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].movie_id, visto);
        assert_eq!(found[0].user, "vitor");
        assert_eq!(found[0].at, at("2026-09-01T20:00:00Z"));
        assert_eq!(store.movies().await.unwrap().len(), 2);
        db.drop().await;
    }
}
