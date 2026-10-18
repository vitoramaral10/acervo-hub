# Para lançar

Tela que transforma "existe num tracker e não no outro" em um envio pronto para
o usuário fazer à mão. O acervo prepara o pacote (checagem de regras, título,
descrição, capturas e `.torrent`), o usuário revisa e envia pelo site, e a
tarefa `cross-seed` acha o torrent publicado e semeia sozinha.

No BJ, nada aqui envia ao site: ele proíbe navegação automatizada (regra
1.12), e o envio é sempre do usuário. No Amigos, que não proíbe automação, o
acervo também pode enviar pelo formulário do site, sempre depois da
confirmação do usuário na tela (ver "Envio ao Amigos pelo acervo").

## Princípios

1. **Os arquivos nunca mudam.** O pacote usa os arquivos do torrent de origem
   como estão: mesma pasta raiz, mesmos nomes, mesmos tamanhos. Release que
   precisaria de mudança para passar nas regras (legenda a separar, faixa a
   tirar, nome com referência a site) é **não elegível**, com o motivo. Isso
   respeita "não alterar o release original" (Amigos 4.0.1, BJ 4.1.1) e mantém
   o layout que `crossseed::same_layout` exige para achar o publicado.
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
   `startSeeding = false`, `trackers = announce`; acompanha
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
  concluídos e 3 seeders além dele: Amigos 4.7 e H&R 18.0 i, de 16/10). `launches.state` passa a `semeando` só com o irmão conferido
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
- **3.2** arquivo compactado (`.rar`, `.zip`, `.7z`, `.r00`...) → Fail.
- **4.0.1** o nome de cada vídeo (o arquivo, não a pasta) contradiz o arquivo
  lido pelo ffprobe → Fail, citando o arquivo quando o pacote tem mais de um
  vídeo. O site valida o nome contra o MediaInfo e renomear é proibido, então
  o pacote fica inelegível antes do envio. Ver "Nome contra o arquivo" abaixo.
- **4.0.1 (padrão de nome)** o nome de cada vídeo precisa trazer um codec de
  áudio e um de vídeo na grafia de release padrão, ou o formulário o recusa
  ("Nome fora do padrão") e renomear é proibido → Fail, só no Amigos (o BJ não
  muda). Áudio (sem diferenciar maiúsculas; canais colados ou após ponto):
  `DDP5.1`/`DDP.5.1`/`DD5.1`/`DD.5.1`/`DD+5.1`/`AAC2.0`/`AAC`/`AC3`/`EAC3`/
  `DTS`/`DTS-HD.MA.5.1`/`DTS-HD`/`DTS-X`/`TrueHD`/`FLAC`/`Opus`/`LPCM`/`PCM`;
  Atmos sozinho não conta. Vídeo (grafia exata): `H.264`, `H.265`, `H264`,
  `H265`, `x264`, `x265`, `AVC`, `HEVC`, `AV1`, `VC-1`, `MPEG-2`; `h264`/`h265`
  minúsculos também passam (comuns em WEB-DL), mas `X264`/`X265` com X
  maiúsculo falham ("o nome usa 'X265' em maiúsculas; o padrão é 'x265'"),
  como no Toy Story ...X265-IAMABLE, recusado pelo site. O codec colado num
  `-GRUPO` que ainda é seguido de mais segmentos com ponto (release refeito,
  `…x264-DON.DUAL-xprince00.mkv`) o site não lê: falta o codec de vídeo. Isso
  foi visto no torrent 73347. Colado no grupo final (`H.264-SJB.mkv`), conta. Parser puro em
  `name_check::standard_name_gaps`; é uma segunda linha 4.0.1, independente da
  conferência contra o arquivo.
- **3.3** obra não falada em português sem áudio PT e sem legenda PT
  (embutida ou arquivo) → Fail.
- **5.6** nome de arquivo/pasta com referência a site (lista: `bj-share`,
  `bjshare`, `amigos-share`, `amigosshare`, e `www.`/`.com`/`.net`/`.org`
  dentro do nome) → Fail.
- **3.11 / 4.15** série em andamento (`continuing`): só episódio avulso;
  temporada completa → Fail. Série encerrada (`ended`): só temporada
  completa; episódio avulso → Fail. Temporada que não cobre todos os
  episódios exibidos da temporada no catálogo → Fail.
- **3.9** Dual sem legenda PT completa (embutida ou em arquivo; forçada não
  conta) → Warn: no formulário vai como Dublado, não Dual Áudio. Dual com
  legenda → Ok. Não se aplica a quem não é Dual.
- **4.2.1** duplicidade contra os releases do destino:
  - destino tem Dual e este é só dublado ou só legendado → Fail;
  - entre os de mesma resolução, fonte e tipo de áudio (dual/dublado/
    legendado), na ordem:
    - canais: este é 2.0 e o destino tem 5.1/7.1 → Fail (o 2.0 é
      substituído); este é 5.1/7.1 e o destino é 2.0 → não pesa (este o
      substitui). Os nossos vêm do ffprobe; os do destino, do título;
    - outro codec de vídeo → Warn "avaliar duplicidade";
    - só o codec de áudio difere (pelos dois nomes: DDP/DD+/EAC3, DD/AC3,
      AAC, DTS, DTS-HD, TrueHD...) → Warn: a staff avalia;
    - série em andamento (`continuing`), mesmos codecs: mesmo grupo → Fail;
      diferença de tamanho abaixo de 2 GiB → Fail; outro grupo com 2 GiB ou
      mais → não pesa (a regra permite). Série finalizada e filme seguem o
      item abaixo;
    - tamanho a menos de 10% → Fail "duplicado provável"; senão Warn
      "avaliar duplicidade";
  - nada parecido → Ok.
- **4.14** fonte inferior a WEB-DL (WEBRip, HDTV, CAM, TS...) com WEB-DL Dual
  no destino → Fail.

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
- **4.2.1.2** duplicidade: mesma resolução, fonte e áudio, tamanho a menos de
  10% → Fail; mesma resolução e fonte com diferença pequena → Warn.
- **4.1.7** compactado → Fail.

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

### Nome contra o arquivo (Amigos 4.0.1, BJ 4.1.1)

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

Os dois sites só liberam envio com seeder (Amigos 4.6: sem seed some em
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
  estava.
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

O áudio vem do laudo (`report.audio`: `dual`, `dublado`, `legendado`,
`nacional`, gravado no preparo), e não do `MediaInfo` do site, que chama de
"Original" o vídeo só legendado. Com a regra 3.9, vai Dublado. Pacote de antes
do campo usa a sugestão do site.

A escolha de cada lista é a do próprio formulário: o primeiro nome sugerido
que, normalizado (sem acento, minúsculo, sem espaço nas pontas), é igual ao de
uma opção. Gêneros e idioma vêm do IMDB; o resto, do `MediaInfo`.

Ajustes do acervo:

- **Regra 3.9:** se o laudo tem o aviso da 3.9 (Dual sem legenda PT), o áudio
  vai como Dublado.
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

