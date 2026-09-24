//! Filmes e perfis de qualidade do gerenciador de filmes, para espelhar no
//! catálogo.
//!
//! Formato conferido contra uma instância real (v6), não só contra a
//! documentação. Campos além dos que identificam o filme são opcionais: uma
//! diferença de versão num deles não pode tornar o catálogo inteiro ilegível.

use serde::Deserialize;

use crate::{ArrClient, ArrError};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMovie {
    pub id: i64,
    pub tmdb_id: u32,
    pub title: String,
    pub path: String,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub original_title: Option<String>,
    #[serde(default)]
    pub original_language: Option<Named>,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub minimum_availability: Option<String>,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub quality_profile_id: Option<i64>,
    #[serde(default)]
    pub added: Option<String>,
    #[serde(default)]
    pub movie_file: Option<RemoteMovieFile>,
    #[serde(default)]
    pub runtime: u32,
    #[serde(default)]
    pub secondary_year: Option<u16>,
    #[serde(default)]
    pub clean_title: Option<String>,
    #[serde(default)]
    pub is_available: bool,
    #[serde(default)]
    pub alternate_titles: Vec<RemoteTitle>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteTitle {
    pub title: String,
}

/// Tamanho por minuto aceito para uma qualidade, em megabytes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteQualityDefinition {
    pub quality: RemoteQuality,
    #[serde(default)]
    pub min_size: Option<f64>,
    #[serde(default)]
    pub max_size: Option<f64>,
    #[serde(default)]
    pub preferred_size: Option<f64>,
}

/// Um download na fila, com a qualidade e o filme dono.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteQueueItem {
    #[serde(default)]
    pub movie_id: Option<i64>,
    #[serde(default)]
    pub quality: Option<RemoteQualityModel>,
    #[serde(default)]
    pub tracked_download_state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QueuePage {
    #[serde(default)]
    records: Vec<RemoteQueueItem>,
}

/// Um release enviado ao cliente, do histórico.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGrab {
    pub source_title: String,
    /// RFC 3339; compara como texto.
    pub date: String,
}

/// Raiz da biblioteca e o espaço livre nela.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRootFolder {
    pub path: String,
    #[serde(default)]
    pub free_space: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMovieFile {
    pub relative_path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub quality: Option<RemoteQualityModel>,
    #[serde(default)]
    pub languages: Vec<Named>,
    #[serde(default)]
    pub release_group: Option<String>,
    #[serde(default)]
    pub edition: Option<String>,
    #[serde(default)]
    pub scene_name: Option<String>,
    #[serde(default)]
    pub date_added: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteQualityModel {
    pub quality: RemoteQuality,
    #[serde(default)]
    pub revision: Option<RemoteRevision>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteQuality {
    pub id: u8,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRevision {
    #[serde(default)]
    pub version: u8,
    #[serde(default)]
    pub real: u8,
    #[serde(default)]
    pub is_repack: bool,
}

/// `{ "id": 1, "name": "English" }`, o formato de idioma da API.
#[derive(Debug, Clone, Deserialize)]
pub struct Named {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteQualityProfile {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub upgrade_allowed: bool,
    #[serde(default)]
    pub cutoff: Option<i64>,
    #[serde(default)]
    pub language: Option<Named>,
    #[serde(default)]
    pub items: Vec<RemoteProfileItem>,
    #[serde(default)]
    pub min_format_score: i32,
    #[serde(default)]
    pub cutoff_format_score: i32,
}

/// Uma qualidade (`quality` preenchido) ou um grupo delas (`items`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProfileItem {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub quality: Option<RemoteQuality>,
    #[serde(default)]
    pub items: Vec<RemoteProfileItem>,
    #[serde(default)]
    pub allowed: bool,
}

impl ArrClient {
    /// Todos os filmes, com o arquivo de cada um.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn movies(&self) -> Result<Vec<RemoteMovie>, ArrError> {
        let path = "api/v3/movie";
        self.get(self.url(path)?, &[], path).await
    }

    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn quality_profiles(&self) -> Result<Vec<RemoteQualityProfile>, ArrError> {
        let path = "api/v3/qualityprofile";
        self.get(self.url(path)?, &[], path).await
    }

    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn quality_definitions(&self) -> Result<Vec<RemoteQualityDefinition>, ArrError> {
        let path = "api/v3/qualitydefinition";
        self.get(self.url(path)?, &[], path).await
    }

    /// Downloads na fila, com o filme de cada um.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn movie_queue(&self) -> Result<Vec<RemoteQueueItem>, ArrError> {
        let path = "api/v3/queue";
        let page: QueuePage = self
            .get(
                self.url(path)?,
                &[("page", "1"), ("pageSize", "1000")],
                path,
            )
            .await?;
        Ok(page.records)
    }

    /// Configuração de indexadores (tetos, flags, legenda embutida), como a
    /// instância a devolve.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn indexer_config(&self) -> Result<serde_json::Value, ArrError> {
        let path = "api/v3/config/indexer";
        self.get(self.url(path)?, &[], path).await
    }

    /// Configuração de gerenciamento de mídia (propers, espaço livre).
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn media_management_config(&self) -> Result<serde_json::Value, ArrError> {
        let path = "api/v3/config/mediamanagement";
        self.get(self.url(path)?, &[], path).await
    }

    /// Filmes pegos (enviados ao cliente) de um filme, do mais novo ao mais
    /// velho.
    ///
    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn movie_grabs(&self, movie_id: i64) -> Result<Vec<RemoteGrab>, ArrError> {
        let path = "api/v3/history/movie";
        let id = movie_id.to_string();
        let mut grabs: Vec<RemoteGrab> = self
            .get(
                self.url(path)?,
                &[("movieId", id.as_str()), ("eventType", "grabbed")],
                path,
            )
            .await?;
        grabs.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(grabs)
    }

    /// # Errors
    ///
    /// Falha de rede, status não-2xx ou resposta fora do formato.
    pub async fn root_folders(&self) -> Result<Vec<RemoteRootFolder>, ArrError> {
        let path = "api/v3/rootfolder";
        self.get(self.url(path)?, &[], path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorte de uma resposta real, com os campos que o catálogo usa.
    #[test]
    fn le_filme_com_arquivo() {
        let movie: RemoteMovie = serde_json::from_str(
            r#"{"id": 959, "title": "Coração Partido", "originalTitle": "Твоё сердце будет разбито",
                "year": 2026, "tmdbId": 1523145, "imdbId": "tt38190257",
                "path": "/media/movies/Your Heart Will Be Broken (2026) {imdb-tt38190257}",
                "monitored": true, "qualityProfileId": 1, "added": "2026-05-05T19:46:31Z",
                "minimumAvailability": "announced", "status": "released",
                "originalLanguage": {"id": 11, "name": "Russian"}, "images": [], "ratings": {},
                "movieFile": {"relativePath": "Your Heart Will Be Broken (2026) {imdb-tt38190257}.mkv",
                    "size": 4097988099, "dateAdded": "2026-06-18T02:57:07Z",
                    "sceneName": "Your.Heart.Will.Be.Broken.2026.1080p.WEB-DL.AAC2.0.h264-GRUPO",
                    "edition": "", "languages": [{"id": 11, "name": "Russian"}],
                    "quality": {"quality": {"id": 3, "name": "WEBDL-1080p", "source": "webdl",
                        "resolution": 1080, "modifier": "none"},
                        "revision": {"version": 1, "real": 0, "isRepack": false}},
                    "mediaInfo": {}}}"#,
        )
        .unwrap();
        let file = movie.movie_file.unwrap();
        assert_eq!(file.size, 4_097_988_099);
        assert_eq!(file.quality.unwrap().quality.id, 3);
        assert_eq!(movie.original_language.unwrap().name, "Russian");
    }

    #[test]
    fn le_perfil_com_grupo() {
        let profile: RemoteQualityProfile = serde_json::from_str(
            r#"{"id": 1, "name": "Any", "upgradeAllowed": false, "cutoff": 1001,
                "language": {"id": -2, "name": "Original"},
                "items": [
                    {"quality": {"id": 1, "name": "SDTV"}, "items": [], "allowed": true},
                    {"id": 1001, "name": "WEB 1080p", "allowed": true, "items": [
                        {"quality": {"id": 3, "name": "WEBDL-1080p"}, "items": [], "allowed": true},
                        {"quality": {"id": 15, "name": "WEBRip-1080p"}, "items": [], "allowed": true}]}
                ]}"#,
        )
        .unwrap();
        assert_eq!(profile.items[1].items.len(), 2);
        assert_eq!(profile.cutoff, Some(1001));
    }
}
