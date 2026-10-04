//! Decisão de release de episódio: a que série e a quais episódios o release
//! pertence, se ele serve, e quais releases pegar.
//!
//! Mesmo desenho do motor de filmes, com o contrato de `docs/series.md`: o
//! pacote de temporada serve para um episódio só (`wanted` é o que falta
//! dentro dele), não há upgrade e o tamanho se mede por episódio. Os alvos são
//! próprios deste crate; quem chama calcula `aired` e o estado, e a decisão
//! não lê relógio.

use std::cmp::Ordering;
use std::collections::HashSet;

use acervo_parser::{
    Language, ParsedEpisode, QualityModel, clean_series_title, parse_episode_title,
    parse_movie_title,
};

use crate::{
    Indexer, Mode, Profile, Rejection, Release, Settings, find_indexer, languages, rank, specs,
};

/// Duração de episódio quando a série não informa.
const DEFAULT_RUNTIME: u32 = 45;

/// Em que pé está um episódio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeState {
    /// Quero: sem arquivo e sem `skip`.
    Wanted,
    /// Tenho: com arquivo (a qualidade que ele tem).
    Have(QualityModel),
    /// Dispensado: sem arquivo, com `skip`.
    Skipped,
    /// Quero, com grab em andamento.
    Queued,
}

#[derive(Debug, Clone)]
pub struct EpisodeTarget {
    pub id: i64,
    pub season: u16,
    pub number: u16,
    /// Foi ao ar; sem data, não foi.
    pub aired: bool,
    pub state: EpisodeState,
}

/// Uma série da biblioteca, com o que a decisão precisa dela.
#[derive(Debug, Clone)]
pub struct SeriesTarget {
    pub id: i64,
    pub tvdb_id: Option<u32>,
    /// Títulos já limpos por `clean_series_title`: o principal, o original, o
    /// da base de metadados e os alternativos.
    pub titles: Vec<String>,
    /// Ano de estreia: release com ano só casa com a série do mesmo ano.
    pub year: Option<u16>,
    /// Minutos por episódio; zero é desconhecido.
    pub runtime: u32,
    /// Idioma original, para o perfil.
    pub language: Language,
    pub episodes: Vec<EpisodeTarget>,
    /// Numeração de cena (XEM): o release com estes números é, no
    /// catálogo, outro episódio.
    pub scene: Vec<SceneMapping>,
}

/// Um par da numeração de cena: o release sai como
/// `scene_season`/`scene_episode`, e no catálogo é `season`/`episode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneMapping {
    pub scene_season: u16,
    pub scene_episode: u16,
    pub season: u16,
    pub episode: u16,
}

/// Os episódios do catálogo que um par de cena quer dizer. `exists` diz se
/// um par existe como episódio no catálogo.
///
/// - sem mapeamento, o par vale como veio;
/// - com mapeamento que inclui o próprio par (o episódio duplo: S01E01 de
///   cena é E01 e E02), valem todos os alvos;
/// - com mapeamento que não o inclui, o par cru que existe no catálogo vale
///   como está (o catálogo já numera como a cena); só o que não existe é
///   traduzido.
fn scene_pair(
    scene: &[SceneMapping],
    season: u16,
    number: u16,
    exists: &impl Fn(u16, u16) -> bool,
) -> Vec<(u16, u16)> {
    let targets: Vec<(u16, u16)> = scene
        .iter()
        .filter(|m| m.scene_season == season && m.scene_episode == number)
        .map(|m| (m.season, m.episode))
        .collect();
    if targets.is_empty() || (!targets.contains(&(season, number)) && exists(season, number)) {
        vec![(season, number)]
    } else {
        targets
    }
}

/// Os episódios do catálogo que um nome de cena (temporada e números) quer
/// dizer, pela regra de [`scene_pair`] em cada número, sem repetição. Pode
/// cruzar temporada (S01E13 de cena é a S02E01 do catálogo).
#[must_use]
pub fn scene_to_catalog(
    scene: &[SceneMapping],
    season: u16,
    episodes: &[u16],
    exists: impl Fn(u16, u16) -> bool,
) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = Vec::new();
    for &number in episodes {
        for pair in scene_pair(scene, season, number, &exists) {
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    out
}

/// O número de cena de um episódio do catálogo, se ele tem um diferente:
/// o par de cena que, pela regra de [`scene_to_catalog`], vira este
/// episódio. `None` se não há, ou se o próprio número é um deles.
#[must_use]
pub fn catalog_to_scene(
    scene: &[SceneMapping],
    season: u16,
    episode: u16,
    exists: impl Fn(u16, u16) -> bool,
) -> Option<(u16, u16)> {
    let valid: Vec<(u16, u16)> = scene
        .iter()
        .filter(|m| m.season == season && m.episode == episode)
        .map(|m| (m.scene_season, m.scene_episode))
        .filter(|&(s, n)| scene_pair(scene, s, n, &exists).contains(&(season, episode)))
        .collect();
    // O próprio número entre os de cena (o primeiro episódio do duplo):
    // busca-se como está.
    if valid.contains(&(season, episode)) {
        return None;
    }
    valid.first().copied()
}

/// O que foi buscado: release de outra série é recusado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub series: i64,
    pub season: Option<u16>,
    pub episodes: Vec<u16>,
}

/// Um release bloqueado: não se pega de novo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedEpisode {
    pub series: Option<i64>,
    pub title: String,
    pub indexer: Option<String>,
}

/// A decisão sobre um release.
#[derive(Debug, Clone)]
pub struct EpisodeDecision {
    /// Posição do release na lista recebida.
    pub release: usize,
    pub parsed: Option<ParsedEpisode>,
    /// Id da série com que casou.
    pub series: Option<i64>,
    /// Ids dos episódios do alvo que o release cobre.
    pub covers: Vec<i64>,
    /// Os cobertos que estão em Quero e já foram ao ar, sem grab em andamento.
    pub wanted: Vec<i64>,
    pub quality: Option<QualityModel>,
    pub rejections: Vec<Rejection>,
    /// Critérios de preferência, para `pick` ordenar sem ver o motor.
    key: Option<rank::Key>,
}

impl EpisodeDecision {
    #[must_use]
    pub fn approved(&self) -> bool {
        self.rejections.is_empty()
    }
}

/// O motor: a biblioteca de séries, os indexadores e as configurações.
#[derive(Debug, Clone, Copy)]
pub struct EpisodeEngine<'a> {
    pub library: &'a [SeriesTarget],
    pub indexers: &'a [Indexer],
    pub settings: &'a Settings,
    pub blocklist: &'a [BlockedEpisode],
}

impl EpisodeEngine<'_> {
    /// Decide os resultados de uma busca pela série e episódios de `scope`,
    /// na ordem recebida: `pick` é quem escolhe.
    #[must_use]
    pub fn search(&self, scope: &Scope, releases: &[Release], _mode: Mode) -> Vec<EpisodeDecision> {
        releases
            .iter()
            .enumerate()
            .map(|(index, release)| self.decide(index, release, Some(scope)))
            .collect()
    }

    /// Decide releases recentes sem série buscada: cada um é casado com a
    /// biblioteca inteira.
    #[must_use]
    pub fn rss(&self, releases: &[Release]) -> Vec<EpisodeDecision> {
        releases
            .iter()
            .enumerate()
            .map(|(index, release)| self.decide(index, release, None))
            .collect()
    }

    /// Primeiro o `tvdbid` do indexador, depois o título limpo, igual. Com
    /// ano no nome, só a série daquele ano. Mais de uma série casando é
    /// ambíguo, e não casa nenhuma: pegar episódio da série errada é pior que
    /// não pegar.
    fn find_series(&self, parsed: &ParsedEpisode, release: &Release) -> Option<&SeriesTarget> {
        if let Some(tvdb) = release.tvdb_id.filter(|id| *id != 0)
            && let Some(series) = self.library.iter().find(|s| s.tvdb_id == Some(tvdb))
        {
            return Some(series);
        }
        let info = &parsed.series_title_info;
        let clean: Vec<String> = [&parsed.series_title, &info.title, &info.title_without_year]
            .into_iter()
            .chain(&info.all_titles)
            .map(|t| clean_series_title(t))
            .filter(|t| !t.trim().is_empty())
            .collect();
        let year = info.year;
        let mut fits = self.library.iter().filter(|s| {
            s.titles.iter().any(|t| clean.contains(t)) && year.is_none_or(|y| s.year == Some(y))
        });
        match (fits.next(), fits.next()) {
            (Some(series), None) => Some(series),
            _ => None,
        }
    }

    fn decide(&self, index: usize, release: &Release, scope: Option<&Scope>) -> EpisodeDecision {
        let mut decision = EpisodeDecision {
            release: index,
            parsed: None,
            series: None,
            covers: Vec::new(),
            wanted: Vec::new(),
            quality: None,
            rejections: Vec::new(),
            key: None,
        };
        let Some(parsed) =
            parse_episode_title(&release.title).filter(|p| !p.series_title.trim().is_empty())
        else {
            decision.rejections.push(Rejection::UnparsableEpisode);
            return decision;
        };
        decision.quality = Some(parsed.quality);
        let Some(series) = self.find_series(&parsed, release) else {
            decision.rejections.push(Rejection::UnknownSeries);
            decision.parsed = Some(parsed);
            return decision;
        };
        decision.series = Some(series.id);
        decision.rejections = self.evaluate(&mut decision, release, &parsed, series, scope);
        decision.parsed = Some(parsed);
        decision
    }

    fn evaluate(
        &self,
        decision: &mut EpisodeDecision,
        release: &Release,
        parsed: &ParsedEpisode,
        series: &SeriesTarget,
        scope: Option<&Scope>,
    ) -> Vec<Rejection> {
        let mut out = Vec::new();
        if scope.is_some_and(|s| s.series != series.id) {
            out.push(Rejection::WrongSeries);
        }

        if readable(parsed) {
            let covered = covered(parsed, series);
            if covered.is_empty() {
                out.push(Rejection::UnknownEpisode);
            }
            decision.covers = covered.iter().map(|e| e.id).collect();
            state_checks(&covered, &mut decision.wanted, &mut out);
        } else {
            out.push(Rejection::UnparsableEpisode);
        }

        let profile = Profile::automatic(Language::Original);
        let quality = parsed.quality;
        if profile
            .items
            .get(usize::try_from(profile.index(quality.quality)).unwrap_or(usize::MAX))
            .is_none_or(|item| !item.allowed)
        {
            out.push(Rejection::QualityNotWanted(quality.quality));
        }

        let indexer = find_indexer(self.indexers, &release.indexer);
        let found = languages::finish(
            if release.languages.is_empty() {
                parsed.languages.clone()
            } else {
                release.languages.clone()
            },
            release,
            indexer,
            series.language,
        );
        if !matches!(
            series.language,
            Language::Any | Language::Original | Language::Unknown
        ) && !found.contains(&series.language)
        {
            out.push(Rejection::WantedLanguage {
                wanted: series.language,
                found,
            });
        }

        // Tamanho por episódio: o pacote não pode parecer enorme só por
        // trazer a temporada inteira.
        let minutes = i64::from(if series.runtime == 0 {
            DEFAULT_RUNTIME
        } else {
            series.runtime
        });
        let each = release.size / u64::try_from(decision.covers.len().max(1)).unwrap_or(1);
        if !decision.covers.is_empty() {
            specs::size_rejections(self.settings, quality.quality, each, minutes, &mut out);
        }
        if specs::is_sample(release) {
            out.push(Rejection::Sample);
        }

        let subs = parse_movie_title(&release.title).and_then(|m| m.hardcoded_subs);
        out.extend(specs::hardcoded_rejection(self.settings, subs.as_deref()));
        if specs::is_raw(release) {
            out.push(Rejection::Raw);
        }
        out.extend(specs::seeders_rejection(indexer, release));

        if self.blocklist.iter().any(|blocked| {
            blocked.series.is_none_or(|s| s == series.id)
                && blocked.title.eq_ignore_ascii_case(&release.title)
                && blocked
                    .indexer
                    .as_deref()
                    .is_none_or(|i| i.eq_ignore_ascii_case(&release.indexer))
        }) {
            out.push(Rejection::Blocklisted);
        }

        let preferred = self
            .settings
            .definition(quality.quality)
            .and_then(|d| d.preferred_size);
        decision.key = Some(rank::key(
            self.settings,
            self.indexers,
            &profile,
            quality,
            release,
            rank::size_score(preferred, each, Some(minutes)),
        ));
        out
    }
}

/// O nome diz temporada e episódio. Diário, absoluto, especial e pacote de
/// extras ficam de fora por ora.
fn readable(parsed: &ParsedEpisode) -> bool {
    let numbered = !parsed.episodes.is_empty()
        || parsed.full_season
        || parsed.partial_season
        || parsed.multi_season;
    numbered
        && !parsed.is_daily()
        && !parsed.is_absolute_numbering()
        && !parsed.special
        && !parsed.season_extra
}

/// Episódios do alvo que o release cobre. Multi-temporada não traz a lista
/// de temporadas, então cobre todas menos a 0. Episódio avulso passa antes
/// pela numeração de cena; pacote de temporada, não.
fn covered<'a>(parsed: &ParsedEpisode, series: &'a SeriesTarget) -> Vec<&'a EpisodeTarget> {
    let numbered = scene_to_catalog(&series.scene, parsed.season, &parsed.episodes, |s, n| {
        series
            .episodes
            .iter()
            .any(|e| e.season == s && e.number == n)
    });
    series
        .episodes
        .iter()
        .filter(|e| {
            if parsed.multi_season {
                e.season != 0
            } else if parsed.full_season || parsed.partial_season {
                e.season == parsed.season
            } else {
                numbered.contains(&(e.season, e.number))
            }
        })
        .collect()
}

/// Preenche `wanted` e recusa o que não traz nada a pegar. Tenho nunca é
/// motivo: só não conta.
fn state_checks(covered: &[&EpisodeTarget], wanted: &mut Vec<i64>, out: &mut Vec<Rejection>) {
    if covered.is_empty() {
        return;
    }
    let wants =
        |e: &&&EpisodeTarget| matches!(e.state, EpisodeState::Wanted | EpisodeState::Queued);
    if !covered.iter().any(|e| wants(&e)) {
        out.push(Rejection::NothingWanted);
        return;
    }
    let idle: Vec<&&EpisodeTarget> = covered
        .iter()
        .filter(|e| e.state == EpisodeState::Wanted)
        .collect();
    if idle.is_empty() {
        out.push(Rejection::AlreadyQueued);
        return;
    }
    wanted.extend(idle.iter().filter(|e| e.aired).map(|e| e.id));
    if wanted.is_empty() {
        out.push(Rejection::NotAired);
    }
}

/// Escolhe o que pegar: os aprovados, por série, na ordem do `rank` de
/// filmes; depois, guloso. Um release só entra se cobre algum `wanted` ainda
/// não tomado, e leva só esses: o que um escolhido antes já tomou não volta
/// no seguinte.
#[must_use]
pub fn pick(decisions: &[EpisodeDecision]) -> Vec<(&EpisodeDecision, Vec<i64>)> {
    let mut taken = HashSet::new();
    pick_from(decisions, |_| true, &mut taken)
}

/// Pacote de temporada (ou de várias).
fn is_pack(decision: &EpisodeDecision) -> bool {
    decision
        .parsed
        .as_ref()
        .is_some_and(|p| p.full_season || p.multi_season)
}

/// Como [`pick`], mas os pacotes escolhem primeiro: o avulso só leva o que
/// nenhum pacote aprovado cobre. É a busca depois que o tracker apagou o
/// avulso porque o pacote saiu.
#[must_use]
pub fn pick_prefer_pack(decisions: &[EpisodeDecision]) -> Vec<(&EpisodeDecision, Vec<i64>)> {
    let mut taken = HashSet::new();
    let mut chosen = pick_from(decisions, is_pack, &mut taken);
    chosen.extend(pick_from(decisions, |d| !is_pack(d), &mut taken));
    chosen
}

fn pick_from<'a>(
    decisions: &'a [EpisodeDecision],
    keep: impl Fn(&EpisodeDecision) -> bool,
    taken: &mut HashSet<i64>,
) -> Vec<(&'a EpisodeDecision, Vec<i64>)> {
    let approved: Vec<&EpisodeDecision> = decisions
        .iter()
        .filter(|d| d.approved() && keep(d))
        .collect();
    let mut series: Vec<Option<i64>> = Vec::new();
    for decision in &approved {
        if !series.contains(&decision.series) {
            series.push(decision.series);
        }
    }
    let mut chosen = Vec::new();
    for id in series {
        let mut group: Vec<&EpisodeDecision> = approved
            .iter()
            .copied()
            .filter(|d| d.series == id)
            .collect();
        group.sort_by(|a, b| match (&a.key, &b.key) {
            (Some(a), Some(b)) => b.cmp(a),
            _ => Ordering::Equal,
        });
        for decision in group {
            let fresh: Vec<i64> = decision
                .wanted
                .iter()
                .copied()
                .filter(|e| !taken.contains(e))
                .collect();
            if !fresh.is_empty() {
                taken.extend(&fresh);
                chosen.push((decision, fresh));
            }
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use acervo_parser::{Quality, Revision};

    use super::*;
    use crate::QualityDefinition;

    const SHOW: &str = "Some Show";

    fn have() -> EpisodeState {
        EpisodeState::Have(QualityModel {
            quality: Quality::WebDl1080p,
            revision: Revision::default(),
        })
    }

    /// Ids de episódio: temporada * 100 + número.
    fn episode(season: u16, number: u16, state: EpisodeState) -> EpisodeTarget {
        EpisodeTarget {
            id: i64::from(season) * 100 + i64::from(number),
            season,
            number,
            aired: true,
            state,
        }
    }

    fn series(id: i64, title: &str, episodes: Vec<EpisodeTarget>) -> SeriesTarget {
        SeriesTarget {
            id,
            tvdb_id: Some(u32::try_from(id).unwrap() * 1000),
            titles: vec![clean_series_title(title)],
            year: None,
            runtime: 40,
            language: Language::English,
            episodes,
            scene: Vec::new(),
        }
    }

    /// Temporada 1 com E01..E09 tidos e E10 em Quero.
    fn last_missing() -> SeriesTarget {
        let mut episodes: Vec<_> = (1..=9).map(|n| episode(1, n, have())).collect();
        episodes.push(episode(1, 10, EpisodeState::Wanted));
        series(1, SHOW, episodes)
    }

    fn release(title: &str) -> Release {
        Release {
            title: title.into(),
            indexer: "tracker".into(),
            size: 4_000_000_000,
            seeders: Some(10),
            peers: Some(12),
            imdb_id: None,
            tmdb_id: None,
            tvdb_id: None,
            languages: Vec::new(),
            container: None,
            flags: 0,
            age_hours: None,
        }
    }

    fn run(
        library: &[SeriesTarget],
        settings: &Settings,
        scope: Option<&Scope>,
        releases: &[Release],
    ) -> Vec<EpisodeDecision> {
        let indexers = [Indexer {
            name: "tracker".into(),
            priority: 25,
            minimum_seeders: 1,
            multi_languages: Vec::new(),
        }];
        let engine = EpisodeEngine {
            library,
            indexers: &indexers,
            settings,
            blocklist: &[],
        };
        match scope {
            Some(scope) => engine.search(scope, releases, Mode::Automatic),
            None => engine.rss(releases),
        }
    }

    fn rss(library: &[SeriesTarget], releases: &[Release]) -> Vec<EpisodeDecision> {
        run(library, &Settings::default(), None, releases)
    }

    fn scope(series: i64) -> Scope {
        Scope {
            series,
            season: None,
            episodes: Vec::new(),
        }
    }

    fn reasons(decision: &EpisodeDecision) -> Vec<&'static str> {
        decision.rejections.iter().map(Rejection::reason).collect()
    }

    #[test]
    fn ultimo_episodio_so_com_o_pacote() {
        let library = [last_missing()];
        let decisions = rss(&library, &[release("Some.Show.S01.1080p.WEB-DL.x264-GRP")]);
        let decision = &decisions[0];
        assert!(decision.approved(), "{:?}", decision.rejections);
        assert_eq!(decision.covers.len(), 10);
        assert_eq!(decision.wanted, [110]);
        let chosen = pick(&decisions);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].0.release, 0);
        assert_eq!(chosen[0].1, [110]);
    }

    #[test]
    fn pacote_e_avulso_do_mesmo_episodio_pega_um_so() {
        let library = [last_missing()];
        let decisions = rss(
            &library,
            &[
                release("Some.Show.S01.1080p.WEB-DL.x264-GRP"),
                release("Some.Show.S01E10.1080p.WEB-DL.x264-GRP"),
            ],
        );
        assert!(decisions.iter().all(EpisodeDecision::approved));
        assert_eq!(pick(&decisions).len(), 1);
    }

    #[test]
    fn nada_em_quero_com_tenho_ou_dispensado() {
        let library = [series(
            1,
            SHOW,
            vec![episode(1, 1, have()), episode(1, 2, EpisodeState::Skipped)],
        )];
        let decisions = rss(
            &library,
            &[
                release("Some.Show.S01.1080p.WEB-DL.x264-GRP"),
                release("Some.Show.S01E02.1080p.WEB-DL.x264-GRP"),
            ],
        );
        assert_eq!(reasons(&decisions[0]), ["NothingWanted"]);
        assert_eq!(reasons(&decisions[1]), ["NothingWanted"]);
        assert!(decisions[0].wanted.is_empty());
        assert!(pick(&decisions).is_empty());
    }

    #[test]
    fn episodio_que_nao_foi_ao_ar() {
        let mut future = episode(1, 1, EpisodeState::Wanted);
        future.aired = false;
        let library = [series(1, SHOW, vec![future])];
        let decisions = rss(&library, &[release("Some.Show.S01E01.1080p.WEB-DL-GRP")]);
        assert_eq!(reasons(&decisions[0]), ["NotAired"]);
        assert_eq!(decisions[0].covers, [101]);
    }

    #[test]
    fn episodio_ja_em_download() {
        let library = [series(1, SHOW, vec![episode(1, 1, EpisodeState::Queued)])];
        let decisions = rss(&library, &[release("Some.Show.S01E01.1080p.WEB-DL-GRP")]);
        assert_eq!(reasons(&decisions[0]), ["AlreadyQueued"]);
    }

    #[test]
    fn outra_serie_na_busca() {
        let library = [
            series(1, SHOW, vec![episode(1, 1, EpisodeState::Wanted)]),
            series(2, "Other Show", vec![episode(1, 1, EpisodeState::Wanted)]),
        ];
        let decisions = run(
            &library,
            &Settings::default(),
            Some(&scope(1)),
            &[
                release("Other.Show.S01E01.1080p.WEB-DL-GRP"),
                release("Some.Show.S01E01.1080p.WEB-DL-GRP"),
                release("Nothing.Known.S01E01.1080p.WEB-DL-GRP"),
            ],
        );
        assert_eq!(reasons(&decisions[0]), ["WrongSeries"]);
        assert_eq!(decisions[0].series, Some(2));
        assert!(decisions[1].approved(), "{:?}", decisions[1].rejections);
        assert_eq!(reasons(&decisions[2]), ["UnknownSeries"]);
        assert_eq!(pick(&decisions).len(), 1);
    }

    #[test]
    fn casa_por_tvdbid_e_por_titulo_alternativo() {
        let mut target = series(1, SHOW, vec![episode(1, 1, EpisodeState::Wanted)]);
        target.titles.push(clean_series_title("Alt Name"));
        let library = [target];
        let mut by_id = release("Completely.Different.S01E01.1080p.WEB-DL-GRP");
        by_id.tvdb_id = Some(1000);
        let decisions = rss(
            &library,
            &[
                by_id,
                release("Alt.Name.S01E01.1080p.WEB-DL-GRP"),
                release("Completely.Different.S01E01.1080p.WEB-DL-GRP"),
            ],
        );
        assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
        assert!(decisions[1].approved(), "{:?}", decisions[1].rejections);
        assert_eq!(reasons(&decisions[2]), ["UnknownSeries"]);
    }

    #[test]
    fn mesmo_titulo_exige_o_ano_e_ambiguo_nao_casa() {
        let mut old = series(1, "Ghosts", vec![episode(1, 1, EpisodeState::Wanted)]);
        old.year = Some(2019);
        let mut new = series(2, "Ghosts", vec![episode(1, 1, EpisodeState::Wanted)]);
        new.year = Some(2021);
        let library = [old, new];
        let releases = [
            release("Ghosts.2019.S01E01.1080p.WEB-DL-GRP"),
            release("Ghosts.2021.S01E01.1080p.WEB-DL-GRP"),
            release("Ghosts.S01E01.1080p.WEB-DL-GRP"),
            release("Ghosts.2020.S01E01.1080p.WEB-DL-GRP"),
        ];
        let decisions = rss(&library, &releases);
        assert_eq!(decisions[0].series, Some(1));
        assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
        assert_eq!(decisions[1].series, Some(2));
        assert!(decisions[1].approved(), "{:?}", decisions[1].rejections);
        assert_eq!(reasons(&decisions[2]), ["UnknownSeries"]);
        assert_eq!(reasons(&decisions[3]), ["UnknownSeries"]);
        // Nem a série buscada desempata: sem ano, é ambíguo.
        let decisions = run(&library, &Settings::default(), Some(&scope(2)), &releases);
        assert_eq!(reasons(&decisions[0]), ["WrongSeries"]);
        assert!(decisions[1].approved(), "{:?}", decisions[1].rejections);
        assert_eq!(reasons(&decisions[2]), ["UnknownSeries"]);
        // Uma série só com o título: sem ano no nome, casa.
        let library = [series(
            1,
            "Ghosts",
            vec![episode(1, 1, EpisodeState::Wanted)],
        )];
        let decisions = rss(&library, &releases[2..3]);
        assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
    }

    #[test]
    fn tamanho_se_mede_por_episodio() {
        let settings = Settings {
            definitions: vec![QualityDefinition {
                quality: Quality::WebDl1080p,
                min_size: Some(10.0),
                max_size: Some(100.0),
                preferred_size: None,
            }],
            ..Settings::default()
        };
        // 40 min: de 400 MB a 4000 MB por episódio.
        let library = [series(
            1,
            SHOW,
            (1..=10)
                .map(|n| episode(1, n, EpisodeState::Wanted))
                .collect(),
        )];
        let mut pack = release("Some.Show.S01.1080p.WEB-DL.x264-GRP");
        pack.size = 20_000_000_000;
        let mut small = release("Some.Show.S01.1080p.WEB-DL.x264-GRP2");
        small.size = 2_000_000_000;
        let mut single = release("Some.Show.S01E01.1080p.WEB-DL.x264-GRP");
        single.size = 2_000_000_000;
        let decisions = run(&library, &settings, None, &[pack, small, single]);
        assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
        assert_eq!(reasons(&decisions[1]), ["BelowMinimumSize"]);
        assert!(decisions[2].approved(), "{:?}", decisions[2].rejections);
    }

    #[test]
    fn multi_episodio_cobre_dois() {
        let library = [series(
            1,
            SHOW,
            vec![
                episode(1, 3, EpisodeState::Wanted),
                episode(1, 4, EpisodeState::Wanted),
                episode(1, 5, EpisodeState::Wanted),
            ],
        )];
        let decisions = rss(&library, &[release("Some.Show.S01E03E04.1080p.WEB-DL-GRP")]);
        assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
        assert_eq!(decisions[0].covers, [103, 104]);
        assert_eq!(decisions[0].wanted, [103, 104]);
    }

    #[test]
    fn episodio_fora_do_alvo_nao_entra() {
        let library = [series(1, SHOW, vec![episode(1, 1, EpisodeState::Wanted)])];
        let decisions = rss(&library, &[release("Some.Show.S01E01E02.1080p.WEB-DL-GRP")]);
        assert_eq!(decisions[0].covers, [101]);
        let decisions = rss(&library, &[release("Some.Show.S05E01.1080p.WEB-DL-GRP")]);
        assert_eq!(reasons(&decisions[0]), ["UnknownEpisode"]);
    }

    #[test]
    fn diario_e_absoluto_nao_sao_lidos() {
        let library = [series(1, SHOW, vec![episode(1, 1, EpisodeState::Wanted)])];
        let decisions = rss(
            &library,
            &[
                release("Some.Show.2025.10.03.1080p.WEB-DL-GRP"),
                release("[Group] Some Show - 001 [1080p]"),
            ],
        );
        for decision in &decisions {
            assert!(
                reasons(decision).contains(&"UnparsableEpisode"),
                "{:?}",
                decision.rejections
            );
            assert!(decision.covers.is_empty());
        }
    }

    #[test]
    fn guloso_gera_dois_grabs() {
        let mut s1: Vec<_> = (1..=3)
            .map(|n| episode(1, n, EpisodeState::Wanted))
            .collect();
        s1.push(episode(2, 4, have()));
        s1.push(episode(2, 5, EpisodeState::Wanted));
        let library = [series(1, SHOW, s1)];
        let decisions = rss(
            &library,
            &[
                release("Some.Show.S01.1080p.WEB-DL.x264-GRP"),
                release("Some.Show.S02E05.1080p.WEB-DL.x264-GRP"),
                release("Some.Show.S02E04.1080p.WEB-DL.x264-GRP"),
            ],
        );
        let mut chosen: Vec<usize> = pick(&decisions).iter().map(|(d, _)| d.release).collect();
        chosen.sort_unstable();
        assert_eq!(chosen, [0, 1]);
    }

    #[test]
    fn guloso_nao_leva_de_novo_o_que_ja_foi_tomado() {
        let library = [series(
            1,
            SHOW,
            vec![
                episode(1, 1, EpisodeState::Wanted),
                episode(1, 2, EpisodeState::Wanted),
            ],
        )];
        // O avulso vem na frente (PROPER ganha), e o pacote depois.
        let decisions = rss(
            &library,
            &[
                release("Some.Show.S01.1080p.WEB-DL.x264-GRP"),
                release("Some.Show.S01E01.PROPER.1080p.WEB-DL.x264-GRP"),
            ],
        );
        assert!(decisions.iter().all(EpisodeDecision::approved));
        assert_eq!(decisions[0].wanted, [101, 102]);
        let chosen: Vec<(usize, Vec<i64>)> = pick(&decisions)
            .into_iter()
            .map(|(d, wanted)| (d.release, wanted))
            .collect();
        assert_eq!(chosen, [(1, vec![101]), (0, vec![102])]);
    }

    #[test]
    fn numeracao_de_cena_vira_a_do_catalogo() {
        let map = |ss, se, s, e| SceneMapping {
            scene_season: ss,
            scene_episode: se,
            season: s,
            episode: e,
        };
        let scene = [map(1, 13, 2, 1), map(1, 14, 2, 2)];
        let none = |_: u16, _: u16| false;
        assert_eq!(
            scene_to_catalog(&scene, 1, &[12, 13], none),
            [(1, 12), (2, 1)]
        );
        assert_eq!(scene_to_catalog(&[], 1, &[13], none), [(1, 13)]);
        assert_eq!(catalog_to_scene(&scene, 2, 2, none), Some((1, 14)));
        assert_eq!(catalog_to_scene(&scene, 2, 3, none), None);
        // O par cru que existe no catálogo vale como está: o catálogo já
        // numera como a cena.
        let has_s01e13 = |s: u16, n: u16| (s, n) == (1, 13);
        assert_eq!(scene_to_catalog(&scene, 1, &[13], has_s01e13), [(1, 13)]);
        assert_eq!(catalog_to_scene(&scene, 2, 1, has_s01e13), None);

        // O duplo: S01E01 de cena é E01 e E02 no catálogo, e S01E02 de cena
        // é o E03. O par cru existe, mas está entre os alvos: valem todos.
        let double = [map(1, 1, 1, 1), map(1, 1, 1, 2), map(1, 2, 1, 3)];
        let all = |s: u16, n: u16| s == 1 && n <= 3;
        assert_eq!(scene_to_catalog(&double, 1, &[1], all), [(1, 1), (1, 2)]);
        // O E02 do catálogo se busca como S01E01 de cena. O S01E02 de cena,
        // porém, existe como episódio no catálogo: vale como está, e o E03
        // não tem número de cena que o traga.
        assert_eq!(catalog_to_scene(&double, 1, 2, all), Some((1, 1)));
        assert_eq!(scene_to_catalog(&double, 1, &[2], all), [(1, 2)]);
        assert_eq!(catalog_to_scene(&double, 1, 3, all), None);
        assert_eq!(catalog_to_scene(&double, 1, 1, all), None);
        let mut duplo = series(
            1,
            SHOW,
            vec![
                episode(1, 1, EpisodeState::Wanted),
                episode(1, 2, EpisodeState::Wanted),
                episode(1, 3, EpisodeState::Wanted),
            ],
        );
        duplo.scene = double.to_vec();
        let decisions = rss(&[duplo], &[release("Some.Show.S01E01.1080p.WEB-DL-GRP")]);
        assert_eq!(decisions[0].covers, [101, 102]);

        // O release S01E13 de cena é o S02E01 do catálogo.
        let mut target = series(
            1,
            SHOW,
            vec![episode(1, 12, have()), episode(2, 1, EpisodeState::Wanted)],
        );
        target.scene = scene.to_vec();
        let library = [target];
        let decisions = rss(&library, &[release("Some.Show.S01E13.1080p.WEB-DL-GRP")]);
        assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
        assert_eq!(decisions[0].covers, [201]);
        assert_eq!(decisions[0].wanted, [201]);
    }

    #[test]
    fn depois_do_avulso_apagado_o_pacote_vem_primeiro() {
        let library = [series(
            1,
            SHOW,
            vec![
                episode(1, 1, EpisodeState::Wanted),
                episode(1, 2, EpisodeState::Wanted),
            ],
        )];
        // O avulso PROPER ganha no rank e o `pick` comum o pega primeiro.
        let decisions = rss(
            &library,
            &[
                release("Some.Show.S01.1080p.WEB-DL.x264-GRP"),
                release("Some.Show.S01E01.PROPER.1080p.WEB-DL.x264-GRP"),
            ],
        );
        let normal: Vec<usize> = pick(&decisions).iter().map(|(d, _)| d.release).collect();
        assert_eq!(normal, [1, 0]);
        let chosen: Vec<(usize, Vec<i64>)> = pick_prefer_pack(&decisions)
            .into_iter()
            .map(|(d, wanted)| (d.release, wanted))
            .collect();
        assert_eq!(chosen, [(0, vec![101, 102])]);
        // Sem pacote aprovado, o avulso ainda serve.
        let decisions = rss(
            &library,
            &[release("Some.Show.S01E01.PROPER.1080p.WEB-DL.x264-GRP")],
        );
        assert_eq!(pick_prefer_pack(&decisions).len(), 1);
    }
}
