# Para lançar

Tela que transforma "existe num tracker e não no outro" em um envio pronto para
o usuário fazer à mão. O acervo prepara o pacote (checagem de regras, título,
descrição, capturas e `.torrent`), o usuário revisa e envia pelo site, e a
tarefa `cross-seed` acha o torrent publicado e semeia sozinha.

Nos dois sites o acervo também pode enviar pelo formulário do próprio site,
sempre depois da confirmação do usuário na tela (ver "Envio ao Amigos pelo
acervo" e "Envio ao BJ pelo acervo"). No BJ, o envio pelo acervo é decisão do
usuário em 26/10/2026, ciente de que a regra 1.12 do BJ proíbe navegação
automatizada: as requisições são as normais do formulário, com a sessão do
indexador, sem nada para disfarçar a automação. O envio à mão continua
disponível ("Marquei como enviado").

## Princípios

1. **O conteúdo dos arquivos nunca muda.** O pacote usa os arquivos do
   torrent de origem como estão. Release que precisaria de mudança no arquivo
   para passar nas regras (faixa a tirar, remux) é **não elegível**, com o
   motivo. O que muda é só a pasta, sempre com hardlinks numa pasta adaptada:
   no BJ, a legenda PT embutida extraída para `.srt` (4.2.1.5); no Amigos, o
   nome no padrão e o que fica fora do torrent (16.1.4, 15.2.16). O layout
   gravado da pasta adaptada é o que `crossseed::same_layout` usa para achar
   o publicado.
2. **Checagem pelo arquivo, não pelo nome.** Codec, resolução, HDR, faixas de
   áudio e legendas vêm do `ffprobe`; o nome do release só completa (fonte
   WEB-DL/BluRay, grupo).
3. **Segredo não sai.** Announce com passkey, chave do ImgBB e links de
   download nunca vão a JSON, log ou mensagem de erro. O `.torrent` gerado
   leva o announce (é o propósito dele) e só sai pela rota de download.
4. **Pouca carga.** Um pacote é preparado por vez. O hash do `.torrent` é do
   qBittorrent (API `torrentcreator`, qBittorrent 5), que já vê `/media`; o
   acervo só roda `ffprobe` e `ffmpeg` (um quadro por vez).
5. **Busca no tracker é rara.** A checagem de duplicidade reaproveita a
   consulta do cross-seed (mesmo `SearchQuery`, cache de 30 min do `Catalog`).

## Sites

`Site` é chaveado pelo nome do indexador (`IndexerRecord.name`):
`bjsharecustom` → `Site::Bj`, `amigosshare` → `Site::Amigos`. Outro nome não é
destino de lançamento.

## Dados

### Migração 38

```sql
ALTER TABLE cross_seed_checks ADD COLUMN outcome TEXT NOT NULL DEFAULT 'not_found'
  CHECK (outcome IN ('found','not_found','layout_differs','error','skipped'));
CREATE TABLE launches (
  hash TEXT NOT NULL,              -- torrent de origem, minúsculas
  indexer TEXT NOT NULL,           -- destino (IndexerRecord.name)
  state TEXT NOT NULL CHECK (state IN
    ('preparando','inelegivel','pronto','falhou','enviado','semeando','descartado')),
  progress TEXT,                   -- andamento legível enquanto 'preparando'
  report JSONB NOT NULL DEFAULT '{}'::jsonb,   -- Report (checagens + releases do destino)
  fields JSONB NOT NULL DEFAULT '{}'::jsonb,   -- Fields (texto gerado)
  screens JSONB NOT NULL DEFAULT '[]'::jsonb,  -- [{url, thumb}]
  torrent BYTEA,                   -- .torrent gerado (tem passkey: só a rota de download lê)
  error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  sent_at TEXT,
  PRIMARY KEY (hash, indexer)
);
```

Na migração, a consulta antiga com irmão vira `found`; a sem irmão é apagada
(o `found` nulo de antes não distingue busca que falhou, pacote parcial e "não
existe lá") e o par volta a ser procurado, já com o desfecho de verdade.

`outcome` da consulta do cross-seed:
- `found`: achou e adicionou (ou já estava no cliente) o irmão;
- `not_found`: buscou e nenhum release de tamanho parecido existe → **candidato**;
- `layout_differs`: há release de tamanho parecido com arquivos diferentes →
  candidato marcado "parecido no destino" (provável duplicado);
- `error`: busca ou download do `.torrent` falhou (429, prisão, fora do ar);
- `skipped`: sem consulta possível (pacote parcial, filme fora do catálogo,
  nome de série que não parseia).

### Candidatos

Fonte (a mesma de `crossseed::sources`): torrent da categoria do acervo,
completo, com grab não falho, todos os arquivos com prioridade > 0 e
`progress` 1. Destino: indexador habilitado com `Site`, diferente do de origem.
É candidato o par (fonte, destino) cuja última consulta tem `outcome` em
`not_found` ou `layout_differs`, ou que tem linha em `launches`. Par com
`found` preenchido não é candidato (já existe no destino).

## Fluxo de um pacote

`POST /ui/api/biblioteca/para-lancar/{hash}/{indexer}/preparar` grava
`launches.state = 'preparando'` e dispara um job em fundo (um por vez,
`tokio::sync::Semaphore(1)`; o segundo espera com `progress = 'na fila'`).
Cada passo atualiza `progress`. Falha técnica → `falhou` com `error`. Par `preparando` sem job (handler ou
gravação do desfecho que falhou) é resgatado para `falhou` pelas rotas.

1. **Origem.** Lê o torrent no cliente (`torrents/info`, `torrents/files`):
   completo, arquivos inteiros, `content_path`. Exige pasta raiz (vários
   arquivos sem raiz comum, ou `content_path` que não termina nela, é `falhou`:
   o `addTask` empacotaria a pasta de download inteira). Vídeos = arquivos `.mkv`,
   `.mp4`, `.avi`, `.m2ts`, `.ts`; o principal é o maior.
2. **Sonda.** `ffprobe -show_streams -show_format` no vídeo principal (e em
   todos os vídeos de um pacote, para a checagem; os textos usam o principal).
3. **Destino.** `Catalog::search(indexer, crossseed::query(fonte))`: os
   releases que já existem lá para o mesmo filme ou episódio, com qualidade
   parseada (`acervo_parser`). Falha de busca → `falhou` (sem checar dupe não
   há pacote).
4. **Checagem** (`launch::rules`, função pura) → `Report`. Alguma `Fail` →
   `inelegivel` e para aqui.
5. **Texto** (`launch::text`, função pura sobre TMDB pt-BR + sonda) → `Fields`.
6. **Capturas.** 4 quadros a 15%, 35%, 55% e 75% da duração:
   `ffmpeg -ss T -i VIDEO -frames:v 1 -threads 2 -f image2pipe -c:v png -`
   (sem escala: as regras pedem a resolução real), cada um enviado ao ImgBB
   (`POST https://api.imgbb.com/1/upload`, multipart `key` + `image`). Guarda
   `{url, thumb}`; `url` é o link direto `i.ibb.co` (resolução original).
   O ImgBB é **opcional**: sem chave configurada o passo é pulado, o pacote
   segue, `screens` fica vazio, a descrição não leva `[img]` nem instrução
   (ela é texto público no site) e o laudo ganha um aviso `Capturas` para
   baixar os 4 PNG do pacote e anexá-los no formulário. Os instantes saem de `launch::screen_times`, função única do
   preparo e da rota de download. O laudo guarda ainda o vídeo principal
   (`capturas`: arquivo, largura, altura, duração) para a tela e para a rota.
7. **`.torrent`.** Announce do destino: de um torrent do cliente cujo grab é do
   indexador destino, `QbitClient::trackers`, a primeira URL real com status
   2 (funcionando); sem nenhum → `falhou` "nenhum torrent do <site> no cliente
   para ler o announce". Cria pelo qBittorrent:
   `torrentcreator/addTask` com `sourcePath = content_path`, `format = v1`,
   `pieceSize = 0` (automático), `optimizeAlignment = false`, `private = true`,
   `startSeeding = false`, `trackers = announce` e, no BJ, `source = BJ` (o
   campo que o BJ grava no `.torrent` recebido: sem ele o hash muda no site);
   acompanha
   `torrentcreator/status` a cada 5 s (teto 2 h); baixa
   `torrentcreator/torrentFile`; apaga a tarefa (`deleteTask`, com repetição) em qualquer
   desfecho, e antes de criar apaga tarefas do mesmo `sourcePath` que uma queda
   anterior deixou. Confere `acervo_clients::layout(bytes)` == arquivos da origem;
   diferente → `falhou`.
8. `pronto`.

### Ações

- `GET  /ui/api/biblioteca/para-lancar` → lista de candidatos (com estado do
  pacote, sem `torrent`, sem segredo).
- `GET  /ui/api/biblioteca/para-lancar/{hash}/{indexer}` → detalhe (report,
  fields, screens, estado, `tem_torrent`).
- `POST .../preparar` → (re)prepara; permitido em qualquer estado menos
  `preparando` e `semeando`.
- `GET  .../torrent` → `application/x-bittorrent`, `Content-Disposition:
  attachment; filename="<pasta raiz>.torrent"`.
- `POST .../enviado` → `state = 'enviado'`, `sent_at = agora`; apaga a linha
  do par em `cross_seed_checks` se `found` é nulo (o par volta a ser
  procurado primeiro).
- `POST .../descartar` → `descartado` (some da lista padrão; filtro "todos"
  mostra).

### Cross-seed depois do envio

- Par com `launches.state = 'enviado'` e `sent_at` há menos de 7 dias é
  **urgente**: entra antes dos outros em `due`, ignora `MAX_ATTEMPTS` e
  `RETRY_DAYS` e espera só 1 h entre consultas.
- Achado o irmão de um par lançado: o irmão recebe a tag de semear do
  indexador **sempre** (quem envia semeia 7 dias, ou até 5 downloads
  concluídos e 3 seeders além dele no BJ, H&R 18.0 i, de 16/10; no Amigos, 7 dias ou 15 seeders, 13.9). `launches.state` passa a `semeando` só com o irmão conferido
  (100%, iniciado); irmão recém-adicionado espera a conferência, e se ela
  recusa o par segue `enviado` e urgente.
- `run` grava o `outcome` de cada consulta.

## Regras

Cada checagem é `Check { rule: "4.1.10.2", status: Ok|Warn|Fail, message }`
em português, com o número da regra do site. `Warn` não bloqueia: aparece para
o usuário julgar (principalmente duplicidade).

Entradas (`Facts`): sonda de cada vídeo (container, codec de vídeo, largura,
altura, HDR10/HLG/Dolby Vision, duração, faixas de áudio com idioma/codec/
canais, faixas de legenda embutidas com idioma/forçada), arquivos do torrent
(caminho, tamanho; legendas externas `.srt/.ass/.ssa/.sub/.idx` com idioma
pelo nome), release parseado (fonte, resolução, grupo), tipo (filme, episódio,
temporada), idioma original da obra (`Language::from_name` do banco), status
da série (`ended`/`continuing`) e episódios da temporada no catálogo, e os
releases do destino (título, tamanho, qualidade parseada).

"PT" = faixa ou legenda em português (`por`/`pt`/`pob`/`pt-BR`; o ffprobe não
distingue pt-BR de pt-PT: aceitar os dois). "Dual" = áudio PT + original.

### Amigos (`Site::Amigos`)

Regras de 23 e 24/10/2026 (a numeração mudou toda: H&R é a seção 13, Upload a
15, Padrão de Nomenclatura a 16). Desde então **renomear é exigido**: o nome
segue o padrão mesmo que o original seja outro (16.1.4), e colocar no padrão é
a única mudança permitida, sempre mantendo a tag original (16.12.8). Retag
(15.2.26) e nome que simula o que o arquivo não tem (15.2.27) seguem proibidos.

- **15.1.6** arquivo compactado → Fail.
- **15.2.27** nome do vídeo contra o arquivo (ver "Nome contra o arquivo"):
  no máximo Warn, porque o nome no padrão sai do arquivo. No pack de
  temporada cujos episódios dariam nomes diferentes à pasta → Fail (ver
  Estrutura).
- **16.1.4** nome no padrão (`launch/naming.rs`, função pura): título, ano ou
  `SxxEyy`, edição, Hybrid, REPACK, source, sigla de streaming e tags vêm do
  nome original; resolução (16.10), áudio da faixa no idioma original com
  canais e Atmos (16.12), HDR na grafia que o original escolheu (16.9), HFR
  (16.11, `avg_frame_rate` ≥ 50), codec pela source (16.8: WEB-DL `H.264`,
  encode/WEBRip `x264`, REMUX `AVC`) e DUAL/MULTi pelos idiomas do áudio vêm do
  ffprobe. Ordem dos campos da 16.4 (REMUX põe o codec antes do áudio). Título
  sem acento e sem `, : ; ! ? * " ' ( ) / \ | < >`, `&` vira `and` (16.2.4).
  O título vem do catálogo (16.2.1; `Facts.catalog_title`): o original na
  obra em inglês (`Deixe.A.Neve.Cair` vira `Let.It.Snow`) e na brasileira
  (`New.Bandits` vira `Cangaco.Novo`), na grafia do catálogo; a sigla fica
  com os pontos (`s.w.a.t.exiles` vira `S.W.A.T.Exiles`), como a staff pediu
  em 30/10 apesar da 16.2.4, e o ponto fora de sigla sai (`Mr.` vira `Mr`). Na série, o do release fica
  quando começa pelo do catálogo e é mais longo (ano ou país que separa
  séries de mesmo nome). Obra estrangeira de outro idioma: o preparo busca o
  título en-US no TMDB (Sua.Culpa vira Your.Fault); é lacuna quando o TMDB
  não tem tradução (devolve o original, ou um título que some na limpeza do
  nome) ou quando a busca falha ou falta a chave, com aviso no log. O ano é o do
  catálogo (16.2.2; The Tender Bar é 2021, não o 2022 do release). Source de
  disco em 2160p é `UHD.BluRay` mesmo sem "UHD" no nome (16.6). `MA` antes da
  source é a sigla do Movies Anywhere (fica, com Warn 16.7); hífen solto no
  título sai; `-DUAL` no fim não é tag. Sem grupo: `NoGroup`. Segunda tag (`-GRUPO.DUAL-OUTRO`, 16.12.7) é mantida.
  Termo na parte técnica que o acervo não reconhece (`x264 FLAME DUAL-OUTRO`,
  `x264-iFT.DUAL.THYRSO10`, `- By Fulano`) é lacuna: pode ser tag de grupo,
  e descartá-la seria retag (15.2.26). Tag com `@` ou com nome de site
  de torrent (`naming::SITE_TAGS`: LAPUMiA, BLUDV...), mesmo como segunda tag,
  cita outro site → lacuna.
  Única tag de quem só acrescenta o áudio PT ao release de outro grupo
  (`naming::DUAL_ADDERS`: shaggy, RK, GMN, Lannister...) → lacuna: a tag
  original sumiu, e relançar é retag (Big Hero, `x264-shaggy`, removido em
  26/10). Única tag colada no `DUAL`/`MULTi` (`…H.265.DUAL-TOSSATO`) também,
  salvo os grupos que lançam o próprio DUAL (`naming::OWN_DUAL`: PiA, ASC).
  O que o padrão pede e nenhum dos dois diz é lacuna → Fail: filme sem ano,
  "WEB" sem `H.264`/`x264` no nome (não dá para separar WEB-DL de WEBRip),
  nome sem source, codec sem grafia (AV1 fora de WEB-DL, 15.2.4). Sigla de
  streaming fora da lista da 16.7 → Warn **16.7**.
- **Estrutura** (`Report.placements`): o torrent leva só os vídeos principais
  (sem amostra nem pasta de extras) e a legenda PT completa em `.srt`/`.ass`
  de cada um, com o mesmo nome do vídeo (16.19.2); o resto fica de fora
  (Ok **15.2.16** listando). Filme e episódio sem legenda solta vão **sem
  pasta** (15.2.17); temporada vai na pasta do pack, nome padrão da pasta da
  origem (16.19.4); episódios com características diferentes (que dariam
  nomes diferentes à pasta) → Fail **15.2.27**. Em DUAL a regra pede a legenda embutida (16.19.3), mas
  embutir seria remux e DUAL sem legenda PT-BR a staff recusa (Big Hero,
  26/10): a solta vai junto (Ok **16.19.3**). Legenda PT necessária em `.sub`/`.idx`/`.ssa` → Fail
  **15.2.12**. Filme ou episódio com mais de um vídeo principal → Fail
  **15.2.17**. Episódios de grupos diferentes: pasta com `MiXED` (16.17) e
  Warn **15.2.20** (Trumpable). Série sem grupo → Fail **15.2.28**.
  `placements` vazio = a origem já é o torrent do padrão.
- **15.2.11** obra não falada em português sem áudio PT e sem legenda PT
  completa (embutida ou arquivo; a forçada não conta) → Fail.
- **15.1.8** nome de arquivo/pasta com referência a site (`bj-share`,
  `bjshare`, `amigos-share`, `amigosshare`, `www.`/`.com`/`.net`/`.org`
  dentro do nome, `@` e nome de site de torrent como palavra, de
  `naming::SITE_TAGS`: LAPUMiA, BLUDV...) → Fail.
- **15.2.18 / 15.2.19** temporada em exibição (o catálogo tem episódio dela
  que não foi ao ar, `season_pending > 0`): pack → Fail, episódio → Ok.
  Temporada já exibida: pack completo (o que não cobre os episódios exibidos
  → Fail), episódio → Fail. Série em andamento sem episódio futuro no catálogo
  (a data pode só não ter saído) → Warn nos dois.
- **15.2.5** encode de Blu-ray (não REMUX) em 720p, 1080p e 2160p (o 2160p
  tem número próprio desde 28/10: 10000/7000):
  pelo bitrate do vídeo (`bit_rate` ou a tag `BPS` do MKV), abaixo do mínimo
  de animação (3500/6000/7000 kbps) → Warn e a source vai como **BRRip** no nome
  (16.6; a staff mandou renomear e lançar o Frozen II), abaixo do geral
  (5000/8000/10000) → Warn (só animação passa como BluRay). Sem o bitrate do vídeo, vale o médio do arquivo
  inteiro, que o áudio infla: só reprova abaixo do de animação e avisa abaixo
  do geral. O Frozen II 2160p tinha 15,8 Mb/s no arquivo e 5998 kb/s no vídeo.
- **15.2.6** REMUX sem áudio PT → Warn (só se a obra nunca teve dublagem).
- **15.2.8** vídeo fora de MKV → Warn (Trumpable).
- **15.3.6** só a source difere (WEB-DL contra encode de Blu-ray, nenhum
  REMUX), com a mesma resolução e áudio, codec que não diverge e tamanho a
  menos de 10% → Fail: a staff trata como a mesma versão (torrents 58396 e
  Ted 2, apagado em 26/10).
- **Busca no destino:** o Amigos busca só pelo IMDb quando a consulta o traz,
  e o envio antigo sem o IMDb ligado não aparece (The Grinch, torrent 64,
  apagado como duplicado em 26/10). No filme, o preparo também busca pelo
  título em português e pelo original, sem o IMDb, e só soma o release do site
  que começa pelo título e pelo ano (um de folga), para não puxar continuação
  nem outra obra de mesmo nome (`naming::title_year_match`).
- **15.3** duplicidade e substituição contra os releases do destino de mesma
  edição (16.14; edição declarada diferente convive; o título do site nunca
  escreve a edição, então o sem edição entra na comparação). Um DUAL completo (com legenda PT
  embutida) não disputa vaga com só dublado e só legendado (15.3.3). Na ordem:
  - **15.3.1** mesma resolução e tamanho a menos de 1% → Fail: é o mesmo
    arquivo, qualquer que seja a etiqueta do site (Balls Up, 26/10);
  - **15.3.4** source inferior (abaixo de WEB-DL/Blu-ray, BRRip pela 15.2.5,
    ou abaixo de 720p) com uma superior de mesma resolução ou maior no site →
    Fail. Título do site sem source (o parser lê HDTV) ou sem resolução (lê
    480p) conta como superior: o Toy Story 4 BRRip foi apagado contra um
    "HDR WEB-DL" sem resolução (28/10);
  - **15.3.3** só dublado ou só legendado com DUAL na mesma resolução → Fail;
  - **15.3.2** (regra de 28/10) cada título tem vagas por resolução, source e
    codec: 2160p Full Disc 1, REMUX 1, encode 1 (só x265; x264 → Fail),
    WEB-DL 2 (H.265 e H.264); 1080p Full Disc 1, REMUX 1, encode 2 (x264 e
    x265), WEB-DL 2 (H.264 e H.265); 720p 1, qualquer source. Ocupa a vaga o
    release do site na mesma linha e no mesmo codec; título sem codec ou sem
    source ocupa (Cangaço Novo S01, 26/10). O nosso só entra se for melhor
    que cada ocupante, nesta ordem: DUAL completo contra não DUAL; HDR contra
    SDR (fora do 2160p; título sem faixa é SDR); pelo menos 1,5 vez o
    tamanho. Senão → Fail, e a vaga fica com o mais antigo. Entrando, Ok com
    quem ele substitui;
  - **15.3.2** em 2160p o nível da faixa vem antes: DV + HDR, depois só HDR
    ou só DV, depois SDR (pelo título e, no nosso, pelo arquivo; "Dolby
    Vision" por extenso conta). Nível abaixo → Fail; título do site sem faixa
    conta como o nível mais alto. SDR com qualquer HDR da obra no site em
    2160p → Fail. DV + HDR sobre um só HDR ou só DV → Warn: a regra manda
    substituir, mas o título do site já trocou a faixa (Avatar Aang, 26/10);
  - BRRip, WEBRip, HDTV e afins (sem vaga) e resoluções fora de 720p, 1080p e
    2160p: a comparação genérica (mesma resolução, source e áudio; tamanho a
    menos de 10% e mesmo codec → Fail "duplicado provável", senão Warn), na
    regra **15.3.1**;
  - release do destino sem resolução no título conta como de resolução
    desconhecida e entra na comparação (torrent 52763).
- Sai a antiga 3.9: DUAL sem legenda é DUAL (15.2.15), e o envio não troca
  mais o áudio para Dublado.

### BJ (`Site::Bj`)
- **4.1.1** mesma conferência de nome contra o arquivo (renomear é proibido
  no BJ) → Fail. Ver "Nome contra o arquivo" abaixo.
- **4.1.10.2** vídeo que não é MKV → Fail. Até 1080p SDR em H.265 → Warn
  ("só se a fonte não tem x264; proibido se há x264 no BJ" — e Fail se o
  destino já tem um x264 na mesma resolução). 4K em H.264 → Warn.
- **4.2.1.14** fonte fora de WEB/WEB-DL/Blu-ray/Remux → Fail. Resolução
  abaixo de 1080p com release ≥ 1080p no destino → Fail.
- **4.2.1.9** HDR/DV numa resolução menor que a maior do destino → Fail.
- **4.2.1.14.1** áudio além do original + PT → Fail (MULTi só com duas
  dublagens oficiais: o usuário confirma, então Warn quando há duas faixas
  PT ou duas no idioma original).
- **4.2.1.5 / 4.2.1.4** (obra nacional dispensa; áudio sem tag de idioma numa
  obra nacional conta como original, não como legendado) sem áudio PT: precisa de legenda PT **em arquivo
  separado** na mesma pasta → Fail se só embutida ou ausente.
- **4.2.1.10** filme com mais de um vídeo, ou mais de 4 legendas → Fail.
- **4.1.2 / 4.1.3** referência a site no nome (mesma lista do Amigos) → Fail.
- **4.2.1.13** temporada incompleta ou pacote com mais de uma temporada →
  Fail.
- **4.2.1.13 (episódio avulso)** destino com o pacote da mesma temporada na
  mesma resolução, fonte e áudio → Warn (o preparo faz uma segunda busca, pela
  temporada, só para episódio).
- **4.1.20** destino tem Dual e este não é Dual → Fail (obra nacional: Ok).
  Sem Dual: legendado com dublado no destino → Fail; dublado com legendado
  no destino → Warn (prefira um Dual).
- **4.2.1.2** duplicidade: mesma resolução, fonte e áudio, tamanho a menos de
  10% → Fail; mesma resolução e fonte com diferença pequena → Warn.
- **4.1.7** compactado → Fail.
- **4.1.8 / 4.1.10** mais de 999 arquivos (fora Blu-ray em M2TS) ou mais
  de 200 GiB → Fail.
- **4.1.11** vídeos principais (sem amostra nem extras) em resoluções
  diferentes, ou uns com áudio PT e outros sem → Fail; Dual ao lado de
  dublado → Warn.

## Texto (`Fields`)

Tudo em português; cada campo tem botão de copiar na tela.

- `titulo`: "Título em português (Título original)" — sem o original se for
  igual ou se a obra é nacional; sem ano (Amigos 4.5). Série: `" - S01E02"`
  (episódio) ou `" - S01"` (temporada).
- `release`: nome da pasta raiz ou do arquivo, intacto.
- `ano`, `imdb` (URL), `tmdb` (URL), `duracao` ("1 h 52 min"), `tamanho`.
- `sinopse`: overview pt-BR do TMDB (filme, ou episódio/temporada da série
  quando houver; senão a da série). Sem pt-BR → vazio e Warn no report
  ("sinopse em PT não encontrada: escreva à mão; tradutor automático não é
  aceito no BJ, 4.1.6").
- `ficha`: direção (ou criação), elenco (até 8), gêneros, país, título
  original, ano.
- `tecnico`: formato, resolução (`1920x1080`), qualidade (WEB-DL…), vídeo
  (codec, HDR/DV, bit depth), áudio (cada faixa: idioma, codec, canais),
  legendas (embutidas e arquivos, idiomas), duração.
- `descricao`: BBCode juntando sinopse, ficha, técnico e as capturas
  (`[img]url[/img]`), sem citar nenhum outro site (Amigos 1.16).

## Configuração

Seção nova `imagens` com `imgbb_api_key` (segredo: entra em
`config::secrets`, Debug redigido, nunca devolvida pela API). Campo na tela de
Configurações. URL com a chave nunca vai a log: o erro do reqwest é trocado por
mensagem própria. A chave é opcional: sem ela o pacote não falha (passo 6) e as
capturas saem só para baixar.

## Capturas para baixar

O preparo só tira e hospeda as 4 capturas no ImgBB para o **BJ**, cuja
descrição colada à mão as leva. No **Amigos** o passo é pulado: o envio pelo
acervo tira as capturas do vídeo na hora e as sobe ao site, e a descrição de
lá vai sem imagem. Em 26/10, as capturas eram de 1 a mais de 2 minutos de cada
preparo, e o 4K DV em que o ffmpeg estoura o tempo deixava o pacote `falhou`.

O formulário do Amigos tem upload de imagem próprio, e o ImgBB só serve a
miniatura ou uma cópia reduzida no navegador; subir essa versão reduziu as
capturas (1920×1012 em vez de 3840×2024) e a staff reclamou. Por isso:

`GET /ui/api/biblioteca/para-lancar/{hash}/{indexer}/capturas/{n}`
(`n` = 1 a 4, também aceito como `{n}.png` e `{n}.jpg`; mesma auth e cabeçalhos de segurança do `.torrent` e do
`mediainfo.txt`).

- Gera o quadro **na hora** com `mediainfo::screenshot`, no vídeo principal da
  origem (torrent no cliente, maior vídeo), no mesmo instante do preparo
  (15/35/55/75% da duração gravada no laudo; pacote antigo relê pelo ffprobe).
  Resolução real, sem escala. Nada vai a disco.
- **Limite de 5 MiB** (`mediainfo::LIMITE_CAPTURA`): o formulário do Amigos
  só aceita imagem de até 5 MB. Regra, sempre na resolução cheia:
  1. PNG sem perda, no máximo de compressão (`-compression_level 100`, que o
     ffmpeg corta no 9 do zlib, e `-pred mixed`; 8 bits por canal, `rgb24`).
  2. Passou de 5 MiB: JPEG (`mjpeg`, `-pix_fmt yuvj420p`, 8 bits mesmo para
     fonte 10 bits/HDR, sem tonemap nem escala) em `-q:v` 2, 3, 4, 6, 9,
     parando na primeira qualidade que couber.
  3. Nem a 9 coube: devolve a menor e registra aviso no log do hub.
- O formato devolvido é o realmente gerado, qualquer que seja a extensão
  pedida: `Content-Type: image/png` ou `image/jpeg`; `Content-Disposition:
  attachment; filename="<vídeo>.captura-<n>.png|jpg"` (nome sem extensão do
  vídeo, sanitizado). O upload ao ImgBB usa o mesmo resultado (mesmos bytes,
  nome e MIME do formato gerado), então também fica em até 5 MiB.
- Um ffmpeg por vez (semáforo em `mediainfo::screenshot`, vale para o preparo
  também): quem chega depois espera a vez.
- 404: `n` fora de 1 a 4 ou extensão que não seja `.png`/`.jpg`, pacote inexistente, torrent fora do cliente, sem
  vídeo ou vídeo fora do disco. 502: cliente de download fora do ar ou ffmpeg
  falhou (detalhe só no log).

## Deploy

- Dockerfile: copiar também `/ffmpeg` do estágio `mwader/static-ffmpeg`
  (binário `ACERVO_FFMPEG`, padrão `ffmpeg`).
- Compose de produção: limites que caibam um quadro 4K HEVC (conferir no
  deploy).

## Tela

Página "Para lançar" (`#lancar`), entre as principais.
- Abas por destino (BJ, Amigos) e filtro de estado (a preparar, prontos,
  enviados, não elegíveis, todos).
- Linha: título, release, tamanho, origem → destino, selo de estado
  ("parecido no destino" quando `layout_differs`), botão Preparar.
- Diálogo do pacote: checagens agrupadas por status com o número da regra;
  releases do destino; campos com botão copiar; seção "Capturas" com 4 botões
  "Baixar captura N" (link para `/capturas/N`, sem extensão; resolução real ao lado, ex.: "3840×2024 — resolução
  original") e, havendo ImgBB, os links diretos `i.ibb.co` com botão copiar;
  "Baixar .torrent"; "Marquei como enviado"; "Descartar"; enquanto
  `preparando`, o andamento com polling de 3 s.
- Configurações → Imagens: chave do ImgBB (opcional).

## Pasta adaptada (legenda separada para o BJ)

O BJ exige a legenda PT-BR em arquivo separado, ao lado do vídeo (4.2.1.5) e
proíbe muxá-la (4.2.1.4). Release sem áudio PT cuja legenda PT só existe
**embutida** e **em texto** (SubRip, ASS/SSA, mov_text, WebVTT) é adaptado em
vez de recusado. O vídeo continua intacto: o que muda é a pasta.

- **Onde:** `<raiz>/lancamentos/<site>/<nome>/`, onde `<raiz>` é o pai do
  `save_path` da origem (em produção `/media`, o mesmo sistema de arquivos dos
  downloads: hardlink exige isso; `EXDEV` → `falhou` com mensagem). `<site>` é
  `bj`. `<nome>` é a pasta raiz da origem; para origem de arquivo único, o
  nome do arquivo sem a extensão.
- **Conteúdo:** hardlink de **todos** os arquivos da origem, na mesma
  estrutura relativa (para origem de arquivo único, o arquivo vai dentro da
  pasta nova), mais, para cada vídeo sem áudio PT e sem legenda PT externa:
  `<stem do vídeo>.pt-BR.srt` com a faixa PT completa (não forçada) e, se
  houver faixa PT forçada, `<stem>.pt-BR.forced.srt`. Extração:
  `ffmpeg -nostdin -loglevel error -i VIDEO -map 0:<índice> -c:s srt -f srt -`,
  UTF-8, timeout 120 s. Entre várias faixas PT completas, a de título com
  "Brasil"/"Brazil"/"BR" ou idioma `pob`, senão a primeira.
- **Regra:** no BJ, 4.2.1.5 com legenda PT só embutida em texto vira `Ok`
  com mensagem "a legenda PT embutida será extraída para arquivo separado" e
  um `Warn` 4.2.1.6 "confira que a legenda é oficial (não de tradutor)". Só
  embutida em imagem (PGS `hdmv_pgs_subtitle`, VobSub `dvd_subtitle`, DVB) →
  continua `Fail`, dizendo que é imagem. O limite de 4 legendas (4.2.1.10)
  conta as que existirão depois da adaptação. `Report.adaptations` lista o que
  será criado (arquivo, faixa de origem).
- **Fluxo:** passo novo "adaptando a pasta" entre capturas e `.torrent`;
  `sourcePath` do `torrentcreator` passa a ser a pasta adaptada; o layout
  conferido é o da pasta adaptada. `launches.adapted_path` e
  `launches.adapted_layout` (migração 39) guardam a pasta e a lista
  `[[caminho, tamanho]]` relativa ao pai dela.
- **Antes de recriar e ao descartar:** apaga a pasta adaptada do par, mas só
  se estiver dentro de `<raiz>/lancamentos/`, e só os arquivos que o acervo
  criou: vídeos/arquivos que são hardlink (mesmo dev+inode) do arquivo da
  origem, e os `.srt` gerados. Qualquer outra coisa na pasta → não apaga nada
  e registra aviso. Pasta de par em `enviado` ou `semeando` nunca é apagada
  por `preparar` (o irmão semeia dela): `preparar` nesses estados é recusado.
- **Cross-seed:** par lançado com `adapted_layout` compara os candidatos com
  esse layout (não com o da origem) e adiciona o irmão com `save_path` = pai
  da pasta adaptada. O resto do fluxo (parado, conferido, 100%, tag de
  semear) é o mesmo.

### Pasta adaptada do Amigos (nome no padrão)

Com `report.placements` não vazio, o passo "adaptando a pasta" monta
`<raiz>/lancamentos/amigos/<nome no padrão>/` com um hardlink por
`Placement` (`adapt::amigos_plan`): o mesmo inode com o nome novo, nada
copiado e nenhuma legenda extraída. O que não tem `Placement` não entra.

- **Torrent de um arquivo só** (filme ou episódio sem legenda solta, 15.2.17):
  o arquivo fica dentro da pasta nova e o `.torrent` é dele
  (`AdaptationPlan.single`). `launches.adapted_path` guarda o conteúdo do
  torrent (o arquivo, nesse caso) e `adapted_layout` só `[[arquivo,
  tamanho]]`; a pasta a limpar é a de cima (`launch::adapted_dir`), e o
  cross-seed semeia com `save_path` = a pasta nova.
- **MediaInfo:** o formulário confere o nome contra o "Complete name", então,
  com a pasta montada, o relatório é refeito a partir do vídeo renomeado.
- `Fields.release` é o nome no padrão (`adapt::release_name`).
- A limpeza segura é a mesma do BJ: só apaga hardlink da origem (mesmo
  dev+inode), qualquer que seja o nome.
- Pacotes do Amigos preparados antes de 26/10 não têm `placements`: prepare
  de novo antes de enviar.

### Nome contra o arquivo (Amigos 15.2.27, BJ 4.1.1)

`launch/name_check.rs`. O parser de tokens é puro (`NameClaims::parse`): separa
o nome do arquivo por ponto, espaço, hífen, sublinhado, colchetes etc. (mantém
o `+` de `DD+`), sem diferenciar maiúsculas, e lê só o que o nome afirma. Cada
vídeo do pacote é conferido pelo seu próprio nome; nome sem token → Ok.

- **Áudio:** `DD`/`AC3` → alguma faixa `ac3` (só `eac3` não vale);
  `DDP`/`DD+`/`DDPA`/`EAC3`/`E-AC-3` → `eac3`; `AAC` → `aac`; `DTS`,
  `DTS-HD`, `DTS-HD.MA`, `DTS-X` → `dts` (e, se o ffprobe expõe `profile`, o
  perfil MA/HD/X precisa bater; sem perfil basta `dts`); `TrueHD`, `FLAC`,
  `Opus` → o codec correspondente. A mensagem diz a divergência: "o nome diz DD
  (AC-3), mas o áudio é E-AC-3 (Dolby Digital Plus)".
- **Canais:** `5.1` → alguma faixa com 6 canais; `7.1` → 8; `2.0` → 2 (colado
  ao codec, `DDP5.1`, ou solto, `DD.5.1`). Fail se nenhuma faixa bate; sem
  contagem de canais lida, não há o que contradizer.
- **Atmos:** só Warn (o ffprobe não expõe o JOC de forma confiável), salvo se o
  título de uma faixa já diz Atmos. Nunca Fail.
- **Vídeo:** `H.264`/`H264`/`x264`/`AVC` → `h264`; `H.265`/`H265`/`x265`/
  `HEVC` → `hevc`; `AV1` → `av1`.
- **Resolução:** `2160p` → largura >= 3800, altura >= 2160 ou altura >= 2000 em
  imagem larga; `1080p` → largura 1800-2200 ou altura 1000-1100; `720p` →
  largura 1200-1400 ou altura 680-740 (cortes widescreen passam: 3840x1600,
  1920x800).
- **HDR:** `DV`/`DoVi` → Dolby Vision; `HDR10` → HDR10 ou Dolby Vision; `HDR` →
  qualquer HDR. Arquivo HDR sem token HDR no nome → Ok.

## MediaInfo

O formulário do Amigos exige o relatório do MediaInfo em texto ("fonte da
verdade": qualidade, extensão, codecs e resolução saem dele) e o preenche
sozinho a partir dele. O relatório tem de ser do MediaInfo de verdade, não uma
imitação a partir do ffprobe.

- **Binário:** `mediainfo` (CLI da MediaArea) **estático**, compilado num
  estágio próprio do Dockerfile (a imagem final é distroless sem libc) e
  copiado para `/usr/local/bin/mediainfo`; `ACERVO_MEDIAINFO` troca o binário.
  Versão fixada; o estágio confere que o binário é estático e roda.
- **Relatório:** `mediainfo --Output=TEXT <arquivo>` com o diretório de
  trabalho na pasta do vídeo e o argumento sendo só o nome do arquivo, para o
  "Complete name" sair só com o nome (sem caminho do servidor). Timeout 120 s,
  `kill_on_drop`. Gerado para o vídeo principal no passo "texto" do pacote e
  guardado em `Fields.mediainfo`; para o BJ com pasta adaptada, o relatório é o
  mesmo (o vídeo não muda).
- **Tela:** campo "MediaInfo" no pacote, com botão de copiar e "Baixar .txt"
  (`GET .../mediainfo.txt`, `text/plain; charset=utf-8`, `attachment;
  filename="<nome do vídeo sem extensão>.txt"`), para o "Abrir arquivo .txt"
  do formulário.
- O `.txt` nunca entra no `.torrent`.

## Semear antes da aprovação

Os dois sites só liberam envio com seeder (Amigos 15.1.2: sem seed some em
12 h). O torrent publicado ainda não aparece na busca antes da aprovação, então
o cross-seed não o acha a tempo. O acervo semeia o próprio `.torrent` gerado,
que é privado e já leva o announce do destino. O Amigos diz que só converte
torrent não privado, e então o hash publicado tende a ser o mesmo.

- **"Marquei como enviado"** adiciona o `.torrent` gerado ao cliente **parado**,
  categoria do acervo, `save_path` = o da origem (ou o pai da pasta adaptada),
  tags `acervo:cross-seed` + tag de semear do destino; o `verify` do cross-seed
  confere e só inicia com 100%. Já no cliente (mesmo hash) não adiciona de
  novo. Falha ao adicionar não desfaz o `enviado`: vira `error` no pacote e
  aviso na tela.
- **"Enviar o .torrent do site"** (`POST .../torrent-do-site`, corpo = bytes
  do `.torrent`, até 10 MiB): para quando o site troca o hash. Confere
  `layout(bytes)` igual ao do pacote (origem ou pasta adaptada); diferente →
  400 dizendo o que difere. Igual → adiciona do mesmo jeito (parado,
  conferido, só inicia com 100%) e passa o pacote a `enviado` se ainda não
  estava. Antes, o `.torrent` gerado de outro hash que ainda tem o tracker do
  destino sai do cliente (sem os arquivos) e o achado do cross-seed nele é
  esquecido: senão ele seria adotado no lugar do do site. Se o gerado também
  semeia para outro destino (a origem, tag de semear ou tracker de outro
  site), nada sai nem entra, e o erro pede para tirar dele o tracker deste
  site à mão e enviar de novo.
- **Status do tracker no pacote:** o detalhe mostra, para o torrent semeado
  (o gerado ou o do site), o status do tracker do destino (funcionando, não
  registrado, erro), sem a URL. "Não registrado" logo após o envio é
  esperado até o site processar ou aprovar; persistindo, a tela sugere enviar
  o `.torrent` do site.

## Envio ao Amigos pelo acervo

O formulário do Amigos (`/torrents/upload`, Laravel com Inertia e React) é
refeito com a sessão do indexador `amigosshare`. O acervo segue os mesmos
passos do JavaScript do site, e nada sai antes da confirmação na tela.

### Sessão

`Indexer::site_request` (só o Cardigann a implementa) faz GET ou POST com JSON
ou multipart, sempre na origem do indexador, com os mesmos cabeçalhos do
JavaScript do site: `Accept: application/json`, `X-Requested-With` e o
`X-XSRF-TOKEN` lido do cookie. Usa o mesmo orçamento de taxa do indexador.
Faz um novo login uma vez quando o site responde 401 ou 419 (token CSRF
vencido), ou quando redireciona para `/login`. O multipart é montado em bytes,
para a requisição poder ser refeita depois do desafio do Cloudflare.
`Catalog::site_request` respeita a espera por 429 e o disjuntor. Falha de rede,
de login ou 429 contam no disjuntor. Resposta HTTP de erro, como a validação
422, volta para quem chamou.

### Prévia: `GET …/envio-ao-site`

Só para pacote do Amigos `pronto`, sem `site_id` e que o cross-seed não achou
no destino. Faz três leituras, sem publicar nada:

1. `GET /torrents/upload`: as props do Inertia em `<script data-page>`. Para o
   tipo (`filmes` ou `series`, que vem de `report.tipo`, ou do nome do release
   em pacote antigo), elas trazem o perfil: listas com id e nome, quais são
   obrigatórias, `coverRequired`, `screenshotsMin` e `screenshotsMax`, e
   `subtitleWhen`. Também trazem a categoria.
2. `POST /torrents/mediainfo` (`{mediainfo}`): para cada lista, os nomes
   sugeridos (qualidade, áudio, codecs, extensão, idioma), mais largura,
   altura, legenda (`embedded`, `separate` ou `none`) e o relatório.
3. `POST /torrents/lookup` (`{imdb_id}`): tipo, título, ano, gêneros, idioma
   original e a capa já hospedada (`cover.path`).

O áudio é o que o `MediaInfo` do site sugere. Filme só legendado vai como
"Original" no Amigos, e isso está certo. Dual sem legenda continua Dual
(15.2.15). Há uma troca:

- obra nacional: o laudo grava `report.audio` = `nacional`, e o áudio vai
  como Nacional.
- Qualidade: a source do nome no padrão (`BluRay` → Blu-Ray, `BRRip`,
  `WEB-DL`...) vem antes da sugestão do site, que leu o encode de Blu-ray de
  animação do Big Hero como BRRip (26/10). O site sugere Dublado para filme brasileiro, como foi com o
  Authentic Games (73403).

Pacote preparado antes do `report.audio` usa a sugestão do site.

A escolha de cada lista é a do próprio formulário: o primeiro nome sugerido
que, normalizado (sem acento, minúsculo, sem espaço nas pontas), é igual ao de
uma opção. Gêneros e idioma vêm do IMDB; o resto, do `MediaInfo`.

Ajustes do acervo:

- **Legenda:** o campo só vai quando o áudio escolhido está em `subtitleWhen`.
  Se o `MediaInfo` diz `none` e o pacote tem arquivo de legenda, vai
  `separate`.

A prévia lista em `faltando` o que impede o envio:

- lista obrigatória sem opção que bata;
- IMDB de outro tipo (série num pacote de filme, ou o contrário);
- falta de capa, quando ela é obrigatória;
- menos capturas que o mínimo do site.

Os ids das opções nunca ficam fixos no código: são lidos a cada envio.

### Envio: `POST …/envio-ao-site`

Toma a posse do par. Refaz o rascunho (a prévia não é confiada) e recusa com
409 se algo falta. Depois:

1. Tira as `SCREEN_COUNT` capturas do vídeo de origem, nos instantes do
   preparo, e as hospeda uma a uma em `POST /torrents/screenshots` (campo
   `image`, devolve `{path}`).
2. Faz o `POST /torrents` multipart, como o `forceFormData` do Inertia: listas
   em `campo[i]`, booleano `1`/`0` e campo ausente como texto vazio. Os campos
   são:
   - `torrent` (o `.torrent` gerado), `name`, `year`, `imdb_id` e
     `category_id`;
   - `width`, `height` e `mediainfo`;
   - `description`: a `descricao` do pacote sem nenhuma `[img]` e sem a seção "Capturas". O site só aceita imagem hospedada nele, e as capturas já aparecem no campo próprio; repetidas na descrição, ficariam duplicadas;
   - `cover_path` e `screenshot_paths[i]`;
   - `subtitle`, `attribute_ids[i]` e `anonymous=0`.
3. Antes do `POST`, `launches.site_id` (migração 41) recebe a marca
   `pendente`. Se o acervo cair com o envio no ar, o torrent pode ter chegado
   ao site, e a marca recusa um segundo envio por cima.
4. A resposta:
   - **Sucesso:** um redirect para `/torrents/<id>`. O id e o estado
     `enviado` vão ao banco numa escrita só (`mark_launch_uploaded`).
   - **Recusa certa:** um 422 com as mensagens do site, ou o formulário de
     volta. A marca é limpa, a mensagem vira o erro do pacote e o estado fica
     `pronto`.
   - **Sem resposta legível:** rede, sessão ou um status inesperado. A marca
     fica e o erro pede para conferir os envios no site.
     - Se o torrent está lá, o usuário usa "Enviar o .torrent do site".
     - Se não está, "Preparar de novo" limpa a marca e libera o envio.
5. Baixa `/torrents/<id>/download`, porque o site troca o hash do enviado, e
   semeia esse torrent pelo mesmo caminho de "Enviar o .torrent do site":
   confere o layout, grava `site_hash`, adiciona parado e confere antes de
   iniciar. Daqui em diante, qualquer falha é aviso e nunca erro, porque o
   envio já existe no site. É o caso do `.torrent` que o site ainda não
   entrega (aguardando liberação, prisão) e do `.torrent` com outros
   arquivos. O pacote fica `enviado`, com o aviso para o usuário enviar o
   `.torrent` à mão depois.

### Tela

No pacote pronto do Amigos, a seção "Enviar pelo acervo" mostra o botão "Ver o
que vai ao Amigos". Ele carrega a prévia: tipo, nome, ano, IMDB, resolução,
cada lista com a opção escolhida, legenda, capa e capturas, mais o que falta.
O botão "Enviar ao Amigos" fica desligado enquanto falta algo.

## Envio ao BJ pelo acervo

Decisão do usuário em 26/10/2026, ciente de que a regra 1.12 do BJ proíbe
navegação automatizada. O formulário do BJ (`upload.php`, Gazelle) é refeito
com a sessão do indexador `bjsharecustom`, pelo mesmo
`Catalog::site_request` do Amigos e com as mesmas guardas: pacote `pronto`,
sem `site_id`, que o cross-seed não achou no destino, e nada sai antes da
confirmação na tela. Código em `launch/bj.rs`.

### Prévia: `GET …/envio-ao-site`

Duas leituras, sem publicar nada:

1. `GET /upload.php`: o `auth` do formulário (`<input type="hidden"
   name="auth">`). Sem ele a sessão caiu, e a prévia falha pedindo para
   conferir o login do indexador.
2. `GET /ajax.php?action=imdb_title&category=movie|tv&imdbid=tt…&auth=…`, o
   mesmo pedido do `upload.js` ao colar o IMDB (no episódio avulso, também
   `&season=&episode=`). A resposta é JSON no formato do OMDb, com o achado do
   TMDB (`tmdbData.movie_results[0]` no filme, `seriesData` e
   `tmdbSeriesData.tv_results[0]` na série). Valores `N/A` contam como
   ausentes. O log de falha do `site_request` sai sem a query, que leva o
   `auth`.

O tipo vem de `report.tipo` (`filme` → `type=0`; `episodio`/`temporada` →
`type=1`, `tipo=episode|season`, temporada e episódio pelo nome do release).

| Campo | De onde vem |
| --- | --- |
| `title`, `tags`, `year`, `elenco`, `diretorserie`, `datalancamento`, `imdbrating` | `Title` (na série, `seriesData.Title`), `Genre`, `Year` (4 dígitos), `Actors`, `Director`, `Released`, `imdbRating` |
| `titulobrasileiro` | `title` (filme) ou `name` (série) do TMDB, quando difere do original |
| `duracaoHR`, `duracaoMIN` (`duracaotipo=selectbox`) | `Runtime` ("93 min"); sem ele, a duração do laudo (`report.capturas.duracao`) |
| `image` (capa) | `poster_path` do TMDB (caminho relativo ganha `https://image.tmdb.org/t/p/original`); a capa não é enviada pelo acervo |
| `sinopse` | a `sinopse` do pacote (PT-BR do TMDB, 4.1.6); sem ela, o `overview` do TMDB |
| `fichatecnica` | o relatório do MediaInfo do pacote (`fields.mediainfo`) |
| `imdblink`, `validimdb=yes`, `adulto=2` (filme) | o IMDB do pacote; no episódio, o `seriesID` |
| série: `pais`, `avaliacao`, `numtemporadas`, `diretor`, `network` | `Country`, `Rated`, `totalSeasons`, `Writer` (como o `upload.js`), vazio |
| `formato` | `Format` da seção General do MediaInfo: Matroska → MKV, MPEG-4 → MP4 |
| `qualidade` | source do nome, lida só depois do ano, do `SxxEyy` ou da resolução: encode e REMUX de BluRay (`BluRay`, `Blu-ray`, `BDRemux` no nome) → `Blu-ray`, `WEB-DL`, `WEBRip`, `WEB`, `HDTV`, `BRRip`, `BDRip`... REMUX de DVD (ou sem BluRay no nome) bloqueia: "escolha a qualidade à mão" |
| `release` (Streaming) | sigla depois da resolução no nome, por extenso: AMZN Amazon, NF Netflix, DSNP Disney+, iT iTunes, ATVP Apple TV+, HMAX HBO Max, MAX Max, GLBO Globoplay, PCOK Peacock, PMTP Paramount+, HULU Hulu; sem sigla, vazio |
| `audio` | `report.audio`: Dual Áudio, Dublado, Legendado ou Nacional |
| `tipolegenda` | Arquivos Separados quando o torrent leva `.srt`/`.ass`/`.ssa`/`.sub`/`.idx`; Embutida quando há legenda PT só embutida (seção Text do MediaInfo); senão Nenhuma |
| `codecvideo` | a família (264, 265, AV1, VC-1, MPEG-2...) sai do nome e, sem nada nele, do `Format` do vídeo no MediaInfo (AVC/HEVC/AV1); a grafia vem da qualidade: WEB-DL, WEB e REMUX → `H.264`/`H.265`; encode de Blu-ray, WEBRip, HDTV e o resto → `x264`/`x265` |
| `codecaudio` | as faixas do vídeo principal no MediaInfo: um codec só é ele; dois que a lista combina (`AC3/DTS`, `AC3/E-AC-3`, `AC3/AAC`) vão combinados; senão, o da faixa original (a primeira que não é PT). DTS XLL/XBR é `DTS-HD`, `DTS:X` pelo nome comercial, MLP FBA é `TrueHD` |
| `idioma` | `original_language` do TMDB (ou, sem ele, o primeiro de `Language`), na grafia da lista; código sem opção bloqueia citando o código; `pt` com Portugal em `Country` é `Português (pt)`; obra nacional é Português |
| `repack` | `on` quando o nome tem REPACK ou PROPER (checkbox desmarcado não vai) |
| `remaster_title` | `Remux` (REMUX), as edições do nome depois do ano (Director's Cut, Extended Edition, IMAX, Open Matte, Theatrical Cut, Final Cut, Noir Edition, Unrated, Uncut...) e os recursos do MediaInfo do vídeo principal, nesta ordem: `10-bit`, `Dolby Vision`, `HDR10`, `HDR10+` (não leva também HDR10), `HLG`, `Dolby Atmos` (JOC ou nome comercial), `Ultrawide` (largura maior que 2 × altura), separados por ` / `; vazio sem nenhuma; mais de 80 caracteres bloqueia. O checkbox "remaster" da página não tem `name` e não vai |
| `resolucaow`, `resolucaoh` | `Width` e `Height` do vídeo no MediaInfo |
| `anonymous` | desligado (não vai) |

Os valores das listas são texto e saem sempre das opções exatas da página,
fixas em `bj.rs`. A prévia lista em `faltando` o que impede o envio: valor sem
opção que bata (com o motivo), título, ano, gêneros, elenco, data de
lançamento, criador (série), duração, sinopse ou capa ausentes, anime (Animation
com idioma original japonês: vai na categoria Animes, que o acervo não envia), IMDB de outro tipo e menos de 2 capturas. Fica no mesmo formato da
prévia do Amigos (`tipo`, `nome`, `ano`, `imdb`, `largura`, `altura`, `capa`,
`campos`, `capturas`, `faltando`; o tipo de legenda vai em `campos`), e o
`auth` nunca sai nela.

### Envio: `POST …/envio-ao-site`

Toma a posse do par, refaz o rascunho (a prévia não é confiada) e recusa com
409 se algo falta. Depois:

1. Tira as `SCREEN_COUNT` capturas do vídeo de origem, nos instantes do
   preparo, em **PNG puro na resolução cheia** (`mediainfo::screenshot_png`,
   sem o recuo para JPEG dos 5 MiB do Amigos; o limite do BJ é 32 MB,
   `LIMITE_CAPTURA_BJ`, e passar dele é erro), e as sobe uma a uma em
   `POST /ajax.php?action=screen_up` (multipart, campo `file`, como o
   Dropzone). A resposta `{url}` vai em `screenshots[]`.
2. Faz o `POST /upload.php` multipart: `submit=true`, `auth`, `file_input` (o
   `.torrent` gerado), os campos acima na ordem da página, `screenshots` vazio
   e um `screenshots[]` por captura.
3. A marca `pendente` em `site_id` antes do `POST`, como no Amigos.
4. A resposta:
   - **Sucesso:** redirect para `torrents.php?id=<grupo>[&torrentid=<torrent>]`.
     O `site_id` é o `torrentid` quando vem, senão o id do grupo, gravado com
     o estado `enviado` numa escrita só. O BJ, na prática, aceita com uma
     página "Atenção" (HTTP 200) e sem redirect: aí o acervo busca o IMDB
     (`torrents.php?searchstr=<imdb>`, que com um só grupo redireciona para
     ele) e acha a linha do grupo cujo `data-torrentname` é o release ou o
     nome do `.torrent` (`id="torrent<id>"`). Achou, é o `site_id`; não
     achou (busca com vários grupos, rede), segue incerta.
   - **Recusa certa:** só a página do formulário de volta (`id="upload_table"`)
     com o erro que o Gazelle escreve em `<p style="color: red">` lido. A marca
     é limpa e a mensagem vira o erro do pacote.
   - **Incerta:** qualquer outra resposta, incluindo o formulário sem erro lido
     e o redirect para `upload.php`, além de rede ou sessão. A marca fica, e o
     erro pede para conferir os envios no BJ, como no Amigos. Num redirect
     inesperado a mensagem traz só o caminho do `Location`, sem a query (pode
     levar `authkey` ou `torrent_pass`); numa página que não é o formulário,
     o `<title>` dela.
5. Semeia o `.torrent` **gerado** (o mesmo caminho de "Marquei como
   enviado"): o BJ só entrega o `.torrent` publicado com passkey. O gerado
   sai do preparo com `source = BJ`, então o hash é o do site. Sem o
   `source` (pacote preparado antes de 27/10/2026), o BJ grava o campo, o
   hash muda, a resposta é 200 com a página do torrent novo (envio incerto)
   e o gerado fica "Torrent não registrado" no tracker: o conserto é
   "Enviar o .torrent do site". Falha ao semear é aviso, nunca erro.

### Tela

O pacote pronto do BJ ganha a mesma seção "Enviar pelo acervo" do Amigos:
"Ver o que vai ao BJ" carrega a prévia (cada campo com o valor, o que falta)
e "Enviar ao BJ" fica desligado enquanto falta algo.
