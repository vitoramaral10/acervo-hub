# Séries

Como o acervo cuida de séries. O desenho segue o de filmes (busca, decisão, grab, fila
por espaço, importação por hardlink, remoção, sugestão de assistidos), com três diferenças que dão o
motivo de existir:

1. **Apagar um episódio é um clique, e ele não volta.** Cada episódio guarda o motivo de
   não ser buscado (`skip`): `unwanted` (nunca quis), `deleted` (apagado na tela) ou
   `watched` (apagado por assistido, do tempo em que assistido saía sozinho). Só "Quero
   de novo" o devolve à busca.
2. **Pacote de temporada serve para um episódio só.** No fim da temporada o tracker só tem
   o pacote, e às vezes apaga os avulsos. A decisão aceita o pacote se ele cobre ao menos
   um episódio que falta, e o grab manda prioridade zero ao qBittorrent para os arquivos
   que não interessam: baixa-se só o que falta, sem duplicar nada.
3. **Não há upgrade.** Episódio com arquivo nunca é trocado.

## Estado de um episódio

| Estado | Regra | Buscado? |
|---|---|---|
| Quero | sem arquivo, sem `skip` | sim, depois de ir ao ar |
| Tenho | com arquivo | não |
| Dispensado | sem arquivo, com `skip` | nunca |

"Foi ao ar" é `air_date <= hoje` (UTC). Sem data, não foi.

Episódio novo anunciado pelo TMDB entra sem `skip` se a série tem `monitor_new`, e com
`unwanted` se não tem. Especiais (temporada 0) entram sempre `unwanted`.

Ao adicionar uma série, a tela escolhe o que buscar: **tudo que falta**, **só a partir da
última temporada** ou **só os próximos episódios**. O que fica de fora entra `unwanted`.

## Metadados

TMDB, com o `tvdb_id` guardado. O título em inglês (`metadata_title`) dá nome à pasta e
aos arquivos. A tarefa `metadados` atualiza cada série a cada 12 h: só os dados que vêm da
base (nunca pasta, pasta de temporada, `monitor_new` nem prioridade, que são da tela), e os
episódios por `sync_episodes`, que nunca mexe em `skip` nem em arquivo. Série removida no
meio da atualização é pulada.

## Decisão (`acervo-decision`, módulo de episódios)

Pura, sem IO. Recebe a biblioteca de séries e os releases e devolve, por release:

- o casamento: a série (por título limpo contra título, original, `metadata_title` e
  alternativos, ou por `tvdbid` do indexador quando vier) e os episódios que o release
  cobre (temporada + números; pacote cobre a temporada inteira; multi-temporada, todas);
- `wanted`: os cobertos que estão em **Quero** e já foram ao ar;
- as rejeições.

Rejeições próprias de série, além das genéricas reaproveitadas do motor de filmes
(qualidade fora do perfil automático, tamanho, amostra, raw, seeders mínimos, legenda
embutida, bloqueio):

- `UnknownSeries` / `WrongSeries`: não casou, ou casou com outra série que não a buscada;
- `UnparsableEpisode`: o nome não diz temporada nem episódio;
- `NothingWanted`: nenhum episódio coberto está em Quero (todos Tenho ou Dispensado);
- `NotAired`: os em Quero que ele cobre ainda não foram ao ar;
- `AlreadyQueued`: todos os em Quero que ele cobre já estão num grab em andamento.

O tamanho se mede por episódio: tamanho do release dividido pelos episódios que ele cobre,
contra o limite por minuto da qualidade (uma tabela fixa no código, a mesma dos filmes)
vezes a duração do episódio (padrão 45 min).

Escolha: os aprovados vão na ordem do `rank` de filmes (saúde, qualidade, prioridade do
indexador, seeders, tamanho). Em seguida, guloso: pega o primeiro, marca os episódios
`wanted` dele como tomados e segue. Um release só é pego se cobre algum `wanted` ainda
não tomado. Uma busca pode gerar mais de um grab: o pacote da S01 e o avulso da S02E05,
por exemplo.

## Busca

Tarefa `busca` (a mesma dos filmes, que passa a fazer as duas obras). Para cada série com
episódio em Quero já exibido:

- **temporada toda exibida** e com algum Quero: busca de temporada (`t=tvsearch&season=N`),
  que traz o pacote e os avulsos;
- **temporada em andamento**: busca por episódio (`season=N&ep=M`) para cada Quero.

Termo: o `metadata_title`; com `tvdbid` quando o indexador anunciar. Categoria 5000.

RSS: categoria 5000 sem termo, cada release casado com a biblioteca inteira.

Busca interativa pela tela, por episódio, por temporada ou pela série. O grab escolhido à
mão pula a decisão, como nos filmes, mas a seleção de arquivos vale igual.

## Grab e seleção de arquivos

Como nos filmes: baixa o `.torrent`, calcula o hash, manda **parado** com a tag
`acervo:fila` na categoria da biblioteca, grava `series_grabs` (com os episódios
`wanted`) e o histórico, e chama `start_queued`.

Antes de o torrent sair da fila, o acervo **escolhe os arquivos**:

1. lista os arquivos (`torrents/files`);
2. lê cada vídeo com o parser de episódio pelo nome (e pela pasta, se o nome não basta;
   num avulso, o vídeo principal é do episódio do release);
3. arquivo de vídeo cujos episódios cruzam algum `wanted` do grab: prioridade normal;
4. todo o resto (episódio que já se tem ou foi dispensado, legenda, amostra, extra):
   prioridade 0.

Torrent sem metadados ainda (magnet) fica parado até tê-los; a importação refaz a escolha
a cada volta, e ela é idempotente. A fila por espaço usa o que falta baixar, que no
qBittorrent já conta só os arquivos escolhidos.

## Importação

Tarefa `importacao`. Para cada grab de série em andamento cujo torrent terminou os
arquivos escolhidos:

- cada vídeo escolhido vira um hardlink em
  `{pasta da série}/[Season {N}/]{Título} - S{TT}E{EE}[-E{EE}…] - {Título do episódio} {Qualidade}.{ext}`
  (multi-episódio no estilo "prefixed range", título de episódio de multi-episódio unido
  por ` + `);
- **título do episódio é obrigatório**: sem ele, a importação espera a próxima volta, e
  importa com `TBA` depois de 48 h da exibição;
- `ffprobe` lê os idiomas; `add_episode_file` grava e liga os episódios;
- o arquivo antigo que ficar sem episódio sai do disco.

Falhas seguem as dos filmes: torrent sumido do cliente vai para a lista de bloqueio (com
`series_id`) e os episódios são buscados de novo; disco cheio devolve o torrent à fila.
O que o cliente diz uma vez só não derruba o download: `missingFiles` e `error` (com espaço
livre) tentam de novo até persistirem 30 min observados pelo processo; aí o arquivo sumido
vira falha sem bloqueio, com o torrent saindo do cliente, e o erro, falha com bloqueio de 7 dias. "Sem seeds" exige, além do
que o cliente diz, 30 min observados ativo e sem seed — torrent que sai de parado ou da fila
recomeça a contagem. Torrent que o tracker deixou de reconhecer (o avulso apagado quando o
pacote sai) conta como falha de download e leva à busca do pacote. Importação travada por
6 h seguidas avisa pelo Gotify, uma vez por grab.

## Remoção

- **Episódio, temporada ou seleção**: apaga os hardlinks, tira os arquivos do catálogo e
  marca os episódios `deleted`. Arquivo multi-episódio sai inteiro. Depois, cada torrent
  que tinha arquivo com o mesmo inode de um apagado é conferido: se nenhum arquivo dele
  continua com hardlink (`nlink > 1`), ele sai do qBittorrent com os arquivos. Pacote com
  outro episódio ainda na biblioteca fica.
- **Série**: como o filme, com a pasta, os torrents (casamento por inode) e o catálogo.

## Assistidos

Assistido não sai sozinho: vira sugestão na tela "Para apagar", por temporada, a unidade
que se marca. Arquivo de episódio assistido por algum usuário do Jellyfin há mais que a
carência, que não é favorito e cuja série não é favorita, passa na regra; arquivo
multi-episódio só passa quando todos os episódios dele foram assistidos, e só o arquivo
que chegou antes de assistirem (o Jellyfin lembra o assistido de um episódio apagado, e
o mesmo episódio baixado de novo seria sugerido sem chance). A temporada é sugerida
quando todo arquivo dela no disco passa; um favorito, um episódio por ver ou um arquivo
sem data de adição a deixa de fora inteira, porque marcar a temporada apagaria esse
arquivo também.

Marcada a temporada (ou a série inteira) e confirmada a remoção, sai como no "Apagar
temporada": os episódios com arquivo no disco perdem o arquivo e ficam `deleted`, e os
torrents que ficam sem nenhum arquivo em uso saem do qBittorrent.

## Prioridade

Série (ou filme) marcada `prioritario` passa na frente: na fila do acervo os prioritários
escolhem antes (dentro de cada grupo, "menor primeiro, pulando quem não cabe"), no
qBittorrent vão ao topo da fila dele (`topPrio`) ao iniciar e sempre que ficam em
`queuedDL`, e na busca dos que faltam entram primeiro.

## Numeração de cena (XEM)

Tarefa `cena`, uma vez por dia: as séries do catálogo que o XEM mapeia ganham os pares
(temporada e episódio de cena → do catálogo) em `scene_mappings`. Um par de cena pode ter
vários alvos (o episódio duplo), e aí guarda também o de identidade; o par cujo único alvo
é ele mesmo não entra. A decisão, a escolha de arquivos e a importação traduzem o episódio
avulso antes de casar, mas o par cru que existe como episódio no catálogo vale como está
(a menos que ele mesmo esteja entre os alvos, como no duplo). A busca por episódio pede o
número de cena. Pacote de temporada não é traduzido, e o verificar disco também não: o
arquivo da biblioteca já está na numeração do catálogo. O endereço do XEM fica em Servidor (`xem_url`).

## Renomear e verificar disco

- **Renomear** põe os arquivos no nome que a importação daria hoje, só com `rename(2)`:
  mesmo inode (o torrent segue semeando), destino existente é erro, nada sai da pasta, a
  pasta de temporada nasce e a vazia sai, as legendas vão junto. Depois da tarefa
  `metadados`, o que tinha `TBA` e ganhou título é renomeado sozinho (evento `renamed`).
- **Verificar disco** liga o vídeo que o catálogo não conhece e casa sem ambiguidade com
  episódio sem arquivo (na tela, dispensado inclusive, que perde o `skip`), tira do
  catálogo o arquivo que sumiu e registra a legenda solta ao lado de um vídeo conhecido
  (como `disco`). Nunca mexe no disco. A importação roda isso sozinha uma vez por hora,
  nas séries cuja pasta mudou, só ligando, e nunca a episódio dispensado.

## Legendas

Legenda separada no torrent (`.srt`, `.ass`, `.ssa`, `.sub`/`.idx`, `.vtt`) vira hardlink
ao lado do vídeo, como `<stem do vídeo>.<idioma>[.forced].<ext>` (idioma pelo fim do nome,
em `pt-BR`, `pt` ou `en`), e fica em `subtitle_files` com a origem (`importacao` ou
`disco`). Num pacote, a legenda de episódio escolhido baixa junto (casada pelo caminho, ou
pelo vídeo da mesma pasta). Apagar o arquivo apaga as legendas; renomear leva junto, com as
do disco que seguem o nome do vídeo. Quando o arquivo de um filme é trocado, só a legenda
do torrent (da importação, ou com outro link) é trocada ou apagada; a posta à mão fica.

## Torrent desregistrado

Na importação, download ativo há 30 min, incompleto e sem seed conectado tem os trackers
consultados. Todos os trackers reais sem funcionar (estado 4) e algum dizendo, com a frase
inteira, que não conhece o torrent (`unregistered torrent`, `torrent not registered`,
`torrent not found`, `torrent não registrado`), em duas voltas seguidas, é falha de
download como o "sem seeds": sai do cliente só se nenhum arquivo dele tem outro link,
bloqueia por 7 dias e busca de novo; numa série, a nova busca prefere o pacote.
