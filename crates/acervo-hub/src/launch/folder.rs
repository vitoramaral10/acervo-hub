//! A pasta adaptada no disco (legenda separada para o BJ): monta a pasta de um
//! [`AdaptationPlan`] e a apaga sem nunca levar o que o acervo não criou.
//!
//! Tudo aqui é síncrono e mexe em disco: o preparo e o descarte chamam de
//! dentro de `spawn_blocking`.
//!
//! A pasta só tem hardlinks dos arquivos da origem (o mesmo inode: nenhum
//! byte copiado) e os `.srt` extraídos. É isso que [`remove`] confere antes
//! de apagar: arquivo que não é hardlink da origem nem `.srt` gerado faz a
//! limpeza desistir inteira.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use super::adapt::{ADAPTED_DIR, AdaptationPlan};

/// O que foi criado até aqui, para desfazer numa falha.
#[derive(Debug, Default)]
struct Made {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl Made {
    /// Apaga, do último ao primeiro, o que foi criado. Só o que esta montagem
    /// criou: pasta que já existia fica.
    fn undo(&self) {
        for file in self.files.iter().rev() {
            match fs::remove_file(file) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    tracing::warn!(
                        "pasta adaptada: `{}` não foi desfeito: {error}",
                        file.display()
                    );
                }
                _ => {}
            }
        }
        for dir in self.dirs.iter().rev() {
            if let Err(error) = fs::remove_dir(dir) {
                tracing::warn!(
                    "pasta adaptada: `{}` não foi desfeita: {error}",
                    dir.display()
                );
            }
        }
    }
}

/// Caminho relativo só com nomes (nada de `..`, `/` no começo ou `.`): o que
/// vem do torrent não escapa da pasta adaptada.
fn relative(path: &str) -> Result<&Path> {
    let rel = Path::new(path);
    if path.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("caminho inválido na pasta adaptada: `{path}`");
    }
    Ok(rel)
}

/// Garante a pasta `dir`, criando as que faltam até ela e anotando cada uma
/// criada. Pasta que já existe é aceita (o link simbólico em `lancamentos` e
/// abaixo já foi recusado por [`path_refusal`]).
fn ensure_dir(dir: &Path, made: &mut Made) -> Result<()> {
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => bail!("`{}` existe e não é pasta", dir.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("lendo `{}`", dir.display())),
    }
    let parent = dir
        .parent()
        .with_context(|| format!("`{}` não tem pasta acima", dir.display()))?;
    ensure_dir(parent, made)?;
    fs::create_dir(dir).with_context(|| format!("criando `{}`", dir.display()))?;
    made.dirs.push(dir.to_path_buf());
    Ok(())
}

/// O erro do hardlink, com o `EXDEV` dito por extenso: copiar o vídeo dobraria
/// o espaço sem avisar, então o acervo para.
fn link_error(error: io::Error, from: &Path, to: &Path, target: &Path) -> anyhow::Error {
    if error.kind() == io::ErrorKind::CrossesDevices {
        let place = target.parent().and_then(Path::parent).unwrap_or(target);
        anyhow!(
            "a pasta de lançamentos `{}` fica em outro sistema de arquivos que o download \
             (EXDEV): hardlink impossível, e o acervo não copia o vídeo",
            place.display()
        )
    } else {
        anyhow::Error::new(error).context(format!(
            "ligando `{}` a `{}`",
            to.display(),
            from.display()
        ))
    }
}

/// Monta a pasta do plano: a pasta nova (que não pode existir), um hardlink
/// por arquivo da origem na mesma estrutura e os `.srt` (`subtitles`: a saída
/// de cada extração e o conteúdo). Devolve o layout final, relativo ao pai da
/// pasta. Qualquer erro desfaz o que foi criado.
///
/// # Errors
///
/// Destino que escapa de `<root>/lancamentos` (link simbólico, alias da
/// origem), pasta que já existe, `EXDEV` (download e `lancamentos` em discos
/// diferentes), legenda que faltou extrair ou erro de disco.
pub(crate) fn build(
    plan: &AdaptationPlan,
    root: &Path,
    subtitles: &[(String, Vec<u8>)],
) -> Result<Vec<(String, u64)>> {
    let mut made = Made::default();
    let built = build_into(plan, root, subtitles, &mut made);
    if built.is_err() {
        made.undo();
    }
    built
}

fn build_into(
    plan: &AdaptationPlan,
    root: &Path,
    subtitles: &[(String, Vec<u8>)],
    made: &mut Made,
) -> Result<Vec<(String, u64)>> {
    let target = &plan.target;
    let base = target
        .parent()
        .with_context(|| format!("a pasta adaptada `{}` não tem pai", target.display()))?;
    let origin: Vec<PathBuf> = plan.links.iter().map(|link| link.from.clone()).collect();
    if let Some(reason) = path_refusal(target, root, &origin)
        .with_context(|| format!("lendo `{}`", target.display()))?
    {
        bail!("{reason}");
    }
    ensure_dir(base, made)?;
    // `create_dir`, e não `create_dir_all`: a pasta tem de ser nova, para tudo
    // o que estiver nela ser do acervo.
    match fs::create_dir(target) {
        Ok(()) => made.dirs.push(target.clone()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => bail!(
            "a pasta adaptada `{}` já existe e não foi limpa",
            target.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("criando `{}`", target.display()));
        }
    }
    for link in &plan.links {
        let to = base.join(relative(&link.to)?);
        if let Some(parent) = to.parent() {
            ensure_dir(parent, made)?;
        }
        fs::hard_link(&link.from, &to).map_err(|e| link_error(e, &link.from, &to, target))?;
        made.files.push(to);
    }
    for extraction in &plan.extractions {
        let bytes = subtitles
            .iter()
            .find(|(output, _)| *output == extraction.output)
            .map(|(_, bytes)| bytes)
            .with_context(|| format!("a legenda `{}` não foi extraída", extraction.output))?;
        let to = base.join(relative(&extraction.output)?);
        if let Some(parent) = to.parent() {
            ensure_dir(parent, made)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
            .with_context(|| format!("criando `{}`", to.display()))?;
        made.files.push(to.clone());
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .with_context(|| format!("gravando `{}`", to.display()))?;
    }
    plan.final_layout(|output| {
        fs::symlink_metadata(base.join(output))
            .ok()
            .map(|m| m.len())
    })
    .context("o tamanho de uma legenda gravada não foi lido")
}

/// Se o caminho é de um `.srt` (a extensão, sem olhar maiúsculas).
pub(crate) fn is_srt(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("srt"))
}

/// Como terminou a limpeza da pasta adaptada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cleanup {
    /// Não havia pasta.
    Absent,
    /// Apagada: só tinha o que o acervo criou.
    Removed,
    /// Nada foi apagado, pelo motivo dado.
    Refused(String),
}

/// Resolve `path` mesmo que o fim ainda não exista: o trecho que existe passa
/// por `canonicalize` e o resto (só nomes) é acrescentado.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    let mut rest = Vec::new();
    let mut head = path;
    loop {
        match fs::canonicalize(head) {
            Ok(mut resolved) => {
                resolved.extend(rest.iter().rev());
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (head.parent(), head.file_name()) else {
                    return Err(error);
                };
                rest.push(name.to_owned());
                head = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// A trava de caminho, antes de criar ou apagar: o destino (`target`) tem de
/// estar, já resolvido, estritamente dentro de `<root>/lancamentos` (também
/// resolvida), sem link simbólico em `lancamentos` nem abaixo dela, e não pode
/// coincidir com, conter ou estar contido em nenhum arquivo da origem. É o que
/// impede um alias de caminho (`lancamentos` apontando para o download) de
/// fazer a pasta adaptada ser a própria origem. `Some(motivo)`: recusar.
fn path_refusal(target: &Path, root: &Path, origin: &[PathBuf]) -> io::Result<Option<String>> {
    let shown = target.display();
    let launches = root.join(ADAPTED_DIR);
    let Ok(below) = target.strip_prefix(&launches) else {
        return Ok(Some(format!(
            "a pasta `{shown}` não está dentro de `{}`: nada foi feito",
            launches.display()
        )));
    };
    let mut walked = PathBuf::new();
    let mut parts: Vec<PathBuf> = launches
        .components()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    parts.extend(below.components().map(|c| PathBuf::from(c.as_os_str())));
    let skip = launches.components().count() - 1;
    for (index, part) in parts.into_iter().enumerate() {
        walked.push(part);
        if index < skip {
            // Acima de `lancamentos` (a raiz e o que a leva até lá) vale o que o
            // sistema montou; só `lancamentos` e o que está abaixo não pode ser link.
            continue;
        }
        match fs::symlink_metadata(&walked) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Ok(Some(format!(
                    "`{}` é um link simbólico: a pasta adaptada `{shown}` pode ser um alias \
                     da origem, e nada foi feito",
                    walked.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    let launches_real = resolve(&launches)?;
    let target_real = resolve(target)?;
    if target_real == launches_real || !target_real.starts_with(&launches_real) {
        return Ok(Some(format!(
            "a pasta `{shown}` não fica dentro de `{}`: nada foi feito",
            launches_real.display()
        )));
    }
    for path in origin {
        let origin_real = resolve(path)?;
        if target_real.starts_with(&origin_real) || origin_real.starts_with(&target_real) {
            return Ok(Some(format!(
                "a pasta `{shown}` coincide com a origem `{}` (alias de caminho): nada foi feito",
                path.display()
            )));
        }
    }
    Ok(None)
}

/// A pasta está em `<raiz>/lancamentos/<site>/<nome>`, pelo caminho escrito:
/// absoluto, sem `..` e exatamente dois níveis abaixo de `lancamentos`.
fn inside_launches(target: &Path, root: &Path) -> bool {
    let launches = root.join(ADAPTED_DIR);
    target.is_absolute()
        && target
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && target.strip_prefix(&launches).is_ok_and(|rest| {
            rest.components().count() == 2
                && rest.components().all(|c| matches!(c, Component::Normal(_)))
        })
}

/// Apaga a pasta adaptada `target`, mas só se ela estiver dentro de
/// `<root>/lancamentos/` (também depois de resolver links simbólicos) e só
/// tiver o que o acervo criou: hardlinks (mesmo dispositivo e inode) de um dos
/// arquivos `origin`, e os `.srt` cujo caminho (relativo ao pai da pasta) está
/// em `generated` com o tamanho gravado (`None`: qualquer tamanho). Qualquer
/// outra coisa (arquivo estranho, link simbólico, `.srt` que o acervo não
/// gerou ou que o usuário trocou) faz desistir sem apagar nada.
///
/// # Errors
///
/// Erro de disco ao ler ou apagar.
pub(crate) fn remove(
    target: &Path,
    root: &Path,
    origin: &[PathBuf],
    generated: &HashMap<String, Option<u64>>,
) -> Result<Cleanup> {
    let shown = target.display();
    if !inside_launches(target, root) {
        return Ok(Cleanup::Refused(format!(
            "a pasta `{shown}` não está dentro de `{}`: nada foi apagado",
            root.join(ADAPTED_DIR).display()
        )));
    }
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Cleanup::Absent),
        Err(error) => return Err(error).with_context(|| format!("lendo `{shown}`")),
        Ok(meta) if !meta.is_dir() => {
            return Ok(Cleanup::Refused(format!(
                "`{shown}` não é uma pasta comum (link simbólico ou arquivo): nada foi apagado"
            )));
        }
        Ok(_) => {}
    }
    let origin_paths: Vec<PathBuf> = origin.to_vec();
    match path_refusal(target, root, &origin_paths) {
        Ok(Some(reason)) => {
            return Ok(Cleanup::Refused(
                reason.replace("nada foi feito", "nada foi apagado"),
            ));
        }
        Ok(None) => {}
        Err(error) => return Err(error).with_context(|| format!("lendo `{shown}`")),
    }
    let origin_real: HashSet<PathBuf> = origin
        .iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect();
    let inodes: HashSet<(u64, u64)> = origin
        .iter()
        .filter_map(|path| fs::metadata(path).ok())
        .map(|meta| (meta.dev(), meta.ino()))
        .collect();
    let base = target.parent().unwrap_or(target);
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut foreign = Vec::new();
    walk(target, &mut |path, meta| {
        let rel = path
            .strip_prefix(base)
            .ok()
            .and_then(Path::to_str)
            .map(str::to_owned);
        let ours = if meta.is_dir() {
            dirs.push(path.to_path_buf());
            true
        } else if meta.is_file() {
            // Hardlink da origem: o mesmo inode, mas por outro caminho. O próprio
            // arquivo da origem (alias de caminho) nunca é "do acervo".
            let link = inodes.contains(&(meta.dev(), meta.ino()))
                && fs::canonicalize(path).is_ok_and(|real| !origin_real.contains(&real));
            let subtitle = rel.as_deref().is_some_and(|r| {
                is_srt(r)
                    && generated
                        .get(r)
                        .is_some_and(|size| size.is_none_or(|size| size == meta.len()))
            });
            if link || subtitle {
                files.push(path.to_path_buf());
            }
            link || subtitle
        } else {
            false
        };
        if !ours {
            foreign.push(rel.unwrap_or_else(|| path.display().to_string()));
        }
    })?;
    if !foreign.is_empty() {
        foreign.sort();
        let more = foreign.len().saturating_sub(3);
        let mut listed = foreign
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if more > 0 {
            let _ = write!(listed, " e mais {more}");
        }
        return Ok(Cleanup::Refused(format!(
            "a pasta `{shown}` tem o que o acervo não criou ({listed}): nada foi apagado"
        )));
    }
    for file in &files {
        fs::remove_file(file).with_context(|| format!("apagando `{}`", file.display()))?;
    }
    // Pré-ordem ao contrário: as de dentro antes das de fora.
    for dir in dirs.iter().rev() {
        fs::remove_dir(dir).with_context(|| format!("apagando `{}`", dir.display()))?;
    }
    fs::remove_dir(target).with_context(|| format!("apagando `{shown}`"))?;
    Ok(Cleanup::Removed)
}

/// Percorre a pasta sem seguir link simbólico: cada entrada (pastas antes do
/// que têm dentro) vai a `visit` com o próprio `lstat`.
fn walk(dir: &Path, visit: &mut impl FnMut(&Path, &fs::Metadata)) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("lendo `{}`", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<_>>()
        .with_context(|| format!("lendo `{}`", dir.display()))?;
    entries.sort();
    for path in entries {
        let meta =
            fs::symlink_metadata(&path).with_context(|| format!("lendo `{}`", path.display()))?;
        visit(&path, &meta);
        if meta.is_dir() {
            walk(&path, visit)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::adapt::{Extraction, Link};

    /// Uma pasta temporária só do teste, apagada ao sair.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("acervo-adaptada-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `<raiz>/downloads/Filme/{Filme.mkv, Filme.nfo}` e o plano da pasta
    /// adaptada em `<raiz>/lancamentos/bj/Filme`.
    fn origin(root: &Path) -> (AdaptationPlan, Vec<PathBuf>) {
        let content = root.join("downloads/Filme");
        fs::create_dir_all(&content).unwrap();
        fs::write(content.join("Filme.mkv"), vec![7_u8; 4096]).unwrap();
        fs::write(content.join("Filme.nfo"), b"nfo").unwrap();
        let plan = AdaptationPlan {
            single: false,
            target: root.join("lancamentos/bj/Filme"),
            links: vec![
                Link {
                    from: content.join("Filme.mkv"),
                    to: "Filme/Filme.mkv".to_owned(),
                    size: 4096,
                },
                Link {
                    from: content.join("Filme.nfo"),
                    to: "Filme/Filme.nfo".to_owned(),
                    size: 3,
                },
            ],
            extractions: vec![Extraction {
                video: "Filme/Filme.mkv".to_owned(),
                stream: 3,
                output: "Filme/Filme.pt-BR.srt".to_owned(),
                forced: false,
            }],
        };
        let files = vec![content.join("Filme.mkv"), content.join("Filme.nfo")];
        (plan, files)
    }

    const SRT: &[u8] = b"1\n00:00:00,100 --> 00:00:01,000\nOl\xc3\xa1\n";

    fn subtitles() -> Vec<(String, Vec<u8>)> {
        vec![("Filme/Filme.pt-BR.srt".to_owned(), SRT.to_vec())]
    }

    fn generated() -> HashMap<String, Option<u64>> {
        HashMap::from([("Filme/Filme.pt-BR.srt".to_owned(), Some(SRT.len() as u64))])
    }

    #[test]
    fn monta_a_pasta_com_hardlink_do_video_e_o_srt_extraido() {
        let scratch = Scratch::new("monta");
        let (plan, _) = origin(&scratch.0);
        let layout = build(&plan, &scratch.0, &subtitles()).expect("pasta montada");
        assert_eq!(
            layout,
            vec![
                ("Filme/Filme.mkv".to_owned(), 4096),
                ("Filme/Filme.nfo".to_owned(), 3),
                ("Filme/Filme.pt-BR.srt".to_owned(), SRT.len() as u64),
            ]
        );
        // Hardlink: o mesmo inode, nenhum byte copiado.
        for link in &plan.links {
            let from = fs::metadata(&link.from).unwrap();
            let to = fs::metadata(plan.target.parent().unwrap().join(&link.to)).unwrap();
            assert_eq!(
                (from.dev(), from.ino()),
                (to.dev(), to.ino()),
                "{}",
                link.to
            );
            assert_eq!(to.nlink(), 2);
        }
        assert_eq!(
            fs::read(plan.target.join("Filme.pt-BR.srt")).unwrap(),
            SRT.to_vec()
        );
        // A pasta tem de ser nova: montar de novo por cima recusa e não mexe
        // no que já estava lá.
        let error = build(&plan, &scratch.0, &subtitles()).unwrap_err();
        assert!(format!("{error:#}").contains("já existe"), "{error:#}");
        assert!(plan.target.join("Filme.mkv").exists());
    }

    #[test]
    fn arquivo_unico_ganha_a_pasta_nova_em_volta() {
        let scratch = Scratch::new("unico");
        let download = scratch.0.join("downloads");
        fs::create_dir_all(&download).unwrap();
        fs::write(download.join("Filme.2024.mkv"), vec![1_u8; 100]).unwrap();
        let plan = AdaptationPlan {
            single: false,
            target: scratch.0.join("lancamentos/bj/Filme.2024"),
            links: vec![Link {
                from: download.join("Filme.2024.mkv"),
                to: "Filme.2024/Filme.2024.mkv".to_owned(),
                size: 100,
            }],
            extractions: vec![Extraction {
                video: "Filme.2024.mkv".to_owned(),
                stream: 2,
                output: "Filme.2024/Filme.2024.pt-BR.srt".to_owned(),
                forced: false,
            }],
        };
        let subs = vec![("Filme.2024/Filme.2024.pt-BR.srt".to_owned(), b"x".to_vec())];
        let layout = build(&plan, &scratch.0, &subs).unwrap();
        assert_eq!(
            layout,
            vec![
                ("Filme.2024/Filme.2024.mkv".to_owned(), 100),
                ("Filme.2024/Filme.2024.pt-BR.srt".to_owned(), 1),
            ]
        );
        assert!(plan.target.join("Filme.2024.mkv").is_file());
    }

    #[test]
    fn amigos_arquivo_unico_renomeado_e_o_torrent_e_o_arquivo() {
        let scratch = Scratch::new("amigos-unico");
        let download = scratch.0.join("downloads/Filme.2024");
        fs::create_dir_all(&download).unwrap();
        fs::write(download.join("filme.mkv"), vec![1_u8; 100]).unwrap();
        fs::write(download.join("filme.nfo"), b"nfo").unwrap();
        let name = "Filme.2024.1080p.WEB-DL.DDP5.1.H.264-GRP";
        let plan = AdaptationPlan {
            single: true,
            target: scratch.0.join(format!("lancamentos/amigos/{name}")),
            links: vec![Link {
                from: download.join("filme.mkv"),
                to: format!("{name}/{name}.mkv"),
                size: 100,
            }],
            extractions: Vec::new(),
        };
        let layout = build(&plan, &scratch.0, &[]).unwrap();
        assert_eq!(layout, vec![(format!("{name}.mkv"), 100)]);
        assert_eq!(plan.content(), plan.target.join(format!("{name}.mkv")));
        let renamed = fs::metadata(plan.content()).unwrap();
        let source = fs::metadata(download.join("filme.mkv")).unwrap();
        assert_eq!(renamed.ino(), source.ino(), "hardlink, não cópia");
        // A limpeza reconhece o hardlink renomeado e leva a pasta inteira.
        let origin = vec![download.join("filme.mkv"), download.join("filme.nfo")];
        let cleaned = remove(&plan.target, &scratch.0, &origin, &HashMap::new()).unwrap();
        assert_eq!(cleaned, Cleanup::Removed);
        assert!(!plan.target.exists());
    }

    #[test]
    fn falha_no_meio_desfaz_o_que_criou() {
        let scratch = Scratch::new("desfaz");
        let (mut plan, _) = origin(&scratch.0);
        // O segundo arquivo da origem sumiu: o primeiro hardlink já existe.
        plan.links[1].from = scratch.0.join("downloads/Filme/sumiu.nfo");
        let error = build(&plan, &scratch.0, &subtitles()).unwrap_err();
        assert!(format!("{error:#}").contains("ligando"), "{error:#}");
        assert!(!plan.target.exists(), "a pasta nova não pode ficar");
        assert!(
            !scratch.0.join("lancamentos").exists(),
            "nem as de cima que ela criou"
        );
        assert!(scratch.0.join("downloads/Filme/Filme.mkv").is_file());

        // Legenda que não veio da extração: também desfaz.
        let (plan, _) = origin(&scratch.0);
        assert!(build(&plan, &scratch.0, &[]).is_err());
        assert!(!plan.target.exists());
    }

    #[test]
    fn caminho_do_torrent_nao_escapa_da_pasta() {
        let scratch = Scratch::new("escape");
        let (mut plan, _) = origin(&scratch.0);
        plan.links[1].to = "Filme/../../fora.nfo".to_owned();
        assert!(build(&plan, &scratch.0, &subtitles()).is_err());
        assert!(!scratch.0.join("lancamentos/fora.nfo").exists());
        assert!(!plan.target.exists());
    }

    #[test]
    fn exdev_vira_mensagem_de_discos_diferentes() {
        let error = link_error(
            io::Error::from(io::ErrorKind::CrossesDevices),
            Path::new("/media/downloads/F/F.mkv"),
            Path::new("/media/lancamentos/bj/F/F.mkv"),
            Path::new("/media/lancamentos/bj/F"),
        );
        let message = format!("{error:#}");
        assert!(message.contains("EXDEV"), "{message}");
        assert!(message.contains("/media/lancamentos"), "{message}");
    }

    #[test]
    fn limpeza_apaga_so_hardlinks_da_origem_e_os_srt_gerados() {
        let scratch = Scratch::new("limpa");
        let (plan, origin_files) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        let cleaned = remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap();
        assert_eq!(cleaned, Cleanup::Removed);
        assert!(!plan.target.exists());
        // A origem continua inteira.
        assert_eq!(
            fs::read(scratch.0.join("downloads/Filme/Filme.mkv"))
                .unwrap()
                .len(),
            4096
        );
        // Sem pasta, nada a fazer.
        assert_eq!(
            remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap(),
            Cleanup::Absent
        );
    }

    #[test]
    fn limpeza_recusa_pasta_com_arquivo_estranho_e_nao_apaga_nada() {
        let scratch = Scratch::new("estranho");
        let (plan, origin_files) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        fs::write(plan.target.join("anotacoes.txt"), b"do usuario").unwrap();
        let cleaned = remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap();
        let Cleanup::Refused(reason) = cleaned else {
            panic!("devia recusar: {cleaned:?}");
        };
        assert!(reason.contains("anotacoes.txt"), "{reason}");
        // Nada saiu: nem o hardlink, nem o .srt.
        for name in ["Filme.mkv", "Filme.nfo", "Filme.pt-BR.srt", "anotacoes.txt"] {
            assert!(plan.target.join(name).exists(), "{name}");
        }
    }

    #[test]
    fn limpeza_recusa_copia_que_nao_e_hardlink_e_srt_desconhecido() {
        let scratch = Scratch::new("copia");
        let (plan, origin_files) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        // O vídeo trocado por uma cópia (outro inode) é do usuário.
        fs::remove_file(plan.target.join("Filme.mkv")).unwrap();
        fs::write(plan.target.join("Filme.mkv"), vec![7_u8; 4096]).unwrap();
        assert!(matches!(
            remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap(),
            Cleanup::Refused(_)
        ));
        assert!(plan.target.join("Filme.pt-BR.srt").exists());

        // Um .srt que o acervo não gerou também não é dele.
        let scratch = Scratch::new("srt-alheio");
        let (plan, origin_files) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        fs::write(plan.target.join("Filme.en.srt"), b"x").unwrap();
        assert!(matches!(
            remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap(),
            Cleanup::Refused(_)
        ));
        // Nem o link simbólico, mesmo apontando para a origem.
        fs::remove_file(plan.target.join("Filme.en.srt")).unwrap();
        std::os::unix::fs::symlink(&origin_files[0], plan.target.join("atalho.mkv")).unwrap();
        assert!(matches!(
            remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap(),
            Cleanup::Refused(_)
        ));
        assert!(origin_files[0].is_file());
    }

    #[test]
    fn limpeza_recusa_srt_com_tamanho_diferente_do_gravado() {
        let scratch = Scratch::new("srt-trocado");
        let (plan, origin_files) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        // O usuário trocou a legenda extraída por uma versão corrigida.
        fs::write(
            plan.target.join("Filme.pt-BR.srt"),
            b"corrigida pelo usuario",
        )
        .unwrap();
        let cleaned = remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap();
        assert!(matches!(cleaned, Cleanup::Refused(_)), "{cleaned:?}");
        assert!(plan.target.join("Filme.pt-BR.srt").exists());
        // Sem tamanho gravado (montagem interrompida), o nome basta.
        let any = HashMap::from([("Filme/Filme.pt-BR.srt".to_owned(), None)]);
        let cleaned = remove(&plan.target, &scratch.0, &origin_files, &any).unwrap();
        assert_eq!(cleaned, Cleanup::Removed);
    }

    #[test]
    fn limpeza_recusa_pasta_fora_de_lancamentos() {
        let scratch = Scratch::new("fora");
        let (_, origin_files) = origin(&scratch.0);
        // Uma pasta só com hardlinks, mas fora de `<raiz>/lancamentos/`.
        let outside = scratch.0.join("outra/bj/Filme");
        fs::create_dir_all(&outside).unwrap();
        fs::hard_link(&origin_files[0], outside.join("Filme.mkv")).unwrap();
        for target in [
            outside.clone(),
            // Escrito como se estivesse dentro, mas saindo por `..`.
            scratch.0.join("lancamentos/bj/../../outra/bj/Filme"),
            // A própria `lancamentos`, ou um nível só abaixo dela.
            scratch.0.join("lancamentos"),
            scratch.0.join("lancamentos/bj"),
            // A pasta de download da origem.
            scratch.0.join("downloads/Filme"),
        ] {
            let cleaned = remove(&target, &scratch.0, &origin_files, &generated()).unwrap();
            assert!(
                matches!(cleaned, Cleanup::Refused(_)),
                "{}: {cleaned:?}",
                target.display()
            );
        }
        assert!(outside.join("Filme.mkv").exists());
        assert!(origin_files[0].is_file());
    }

    #[test]
    fn limpeza_recusa_lancamentos_que_aponta_para_fora_por_link_simbolico() {
        let scratch = Scratch::new("link");
        let (_, origin_files) = origin(&scratch.0);
        let elsewhere = scratch.0.join("outra/Filme");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::hard_link(&origin_files[0], elsewhere.join("Filme.mkv")).unwrap();
        fs::create_dir_all(scratch.0.join("lancamentos/bj")).unwrap();
        // A pasta adaptada é um link para outro lugar.
        std::os::unix::fs::symlink(&elsewhere, scratch.0.join("lancamentos/bj/Filme")).unwrap();
        let cleaned = remove(
            &scratch.0.join("lancamentos/bj/Filme"),
            &scratch.0,
            &origin_files,
            &generated(),
        )
        .unwrap();
        assert!(matches!(cleaned, Cleanup::Refused(_)), "{cleaned:?}");
        assert!(elsewhere.join("Filme.mkv").exists());
    }

    #[test]
    fn limpeza_nao_apaga_a_origem_quando_lancamentos_e_link_para_a_raiz() {
        let scratch = Scratch::new("alias-raiz");
        // A origem mora em `<raiz>/bj/Filme` e `lancamentos` aponta para `.`:
        // `lancamentos/bj/Filme` é a própria pasta da origem.
        let content = scratch.0.join("bj/Filme");
        fs::create_dir_all(&content).unwrap();
        fs::write(content.join("Filme.mkv"), vec![7_u8; 4096]).unwrap();
        std::os::unix::fs::symlink(".", scratch.0.join("lancamentos")).unwrap();
        let origin_files = vec![content.join("Filme.mkv")];
        let target = scratch.0.join("lancamentos/bj/Filme");
        let cleaned = remove(&target, &scratch.0, &origin_files, &HashMap::new()).unwrap();
        let Cleanup::Refused(reason) = cleaned else {
            panic!("devia recusar: {cleaned:?}");
        };
        assert!(reason.contains("nada foi apagado"), "{reason}");
        assert_eq!(fs::read(content.join("Filme.mkv")).unwrap().len(), 4096);
    }

    #[test]
    fn hardlink_so_vale_por_caminho_diferente_do_da_origem() {
        let scratch = Scratch::new("alias-arquivo");
        let (plan, origin_files) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        // Um alias de arquivo: o link da pasta adaptada trocado por um caminho
        // que chega à própria origem não passa por hardlink.
        let content = scratch.0.join("downloads/Filme");
        let origin_alias = vec![content.join("../Filme/Filme.mkv"), origin_files[1].clone()];
        let cleaned = remove(&plan.target, &scratch.0, &origin_alias, &generated()).unwrap();
        assert_eq!(cleaned, Cleanup::Removed);
        assert!(origin_files[0].is_file() && origin_files[1].is_file());

        // O caminho da origem é o próprio arquivo dentro da pasta: não é cópia.
        let scratch = Scratch::new("alias-mesmo");
        let (plan, _) = origin(&scratch.0);
        build(&plan, &scratch.0, &subtitles()).unwrap();
        let inside = vec![plan.target.join("Filme.mkv"), plan.target.join("Filme.nfo")];
        let cleaned = remove(&plan.target, &scratch.0, &inside, &generated()).unwrap();
        assert!(matches!(cleaned, Cleanup::Refused(_)), "{cleaned:?}");
        assert!(plan.target.join("Filme.mkv").is_file());
    }

    #[test]
    fn preparo_e_limpeza_recusam_lancamentos_link_para_os_downloads() {
        let scratch = Scratch::new("alias-downloads");
        let (plan, origin_files) = origin(&scratch.0);
        let downloads = scratch.0.join("downloads");
        std::os::unix::fs::symlink(&downloads, scratch.0.join("lancamentos")).unwrap();
        let error = build(&plan, &scratch.0, &subtitles()).unwrap_err();
        assert!(format!("{error:#}").contains("link simbólico"), "{error:#}");
        // Nada nasceu dentro dos downloads.
        assert!(!downloads.join("bj").exists());
        // A limpeza de uma pasta que já existe por esse caminho também recusa.
        fs::create_dir_all(downloads.join("bj/Filme")).unwrap();
        fs::hard_link(&origin_files[0], downloads.join("bj/Filme/Filme.mkv")).unwrap();
        let cleaned = remove(&plan.target, &scratch.0, &origin_files, &generated()).unwrap();
        assert!(matches!(cleaned, Cleanup::Refused(_)), "{cleaned:?}");
        for file in &origin_files {
            assert!(file.is_file());
        }
        assert!(downloads.join("bj/Filme/Filme.mkv").is_file());
    }

    #[test]
    fn preparo_recusa_destino_que_coincide_com_a_origem() {
        let scratch = Scratch::new("coincide");
        let (mut plan, _) = origin(&scratch.0);
        // O destino escrito dentro de `lancamentos` mas a origem é a própria
        // pasta: coincide, contém ou está contida.
        let inside = scratch.0.join("lancamentos/bj/Filme");
        fs::create_dir_all(&inside).unwrap();
        plan.links[0].from = inside.join("Filme.mkv");
        fs::write(&plan.links[0].from, b"x").unwrap();
        let error = build(&plan, &scratch.0, &subtitles()).unwrap_err();
        assert!(
            format!("{error:#}").contains("alias de caminho"),
            "{error:#}"
        );
        assert!(plan.links[0].from.is_file());
    }
}
