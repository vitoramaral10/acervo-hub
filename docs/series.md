# Séries

Como o acervo cuida de séries. O desenho segue o de filmes (busca, decisão, grab, fila
por espaço, importação por hardlink, remoção, assistidos), com três diferenças que dão o
motivo de existir:

1. **Apagar um episódio é um clique, e ele não volta.** Cada episódio guarda o motivo de
   não ser buscado (`skip`): `unwanted` (nunca quis), `deleted` (apagado na tela) ou
   `watched` (assistido no Jellyfin). Só "Quero de novo" o devolve à busca.
2. **Pacote de temporada serve para um episódio só.** No fim da temporada o tracker só tem
   o pacote, e às vezes apaga os avulsos. A decisão aceita o pacote se ele cobre ao menos
   um episódio que falta, e o grab manda prioridade zero ao qBittorrent para os arquivos
   que não interessam: baixa-se só o que falta, sem duplicar nada.
3. **Não há upgrade.** Episódio com arquivo nunca é trocado, como os filmes.

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
aos arquivos, como fazia o gerenciador anterior. A tarefa `metadados` atualiza cada série
a cada 12 h: dados da série, e os episódios por `sync_episodes`, que nunca mexe em `skip`
nem em arquivo.

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
contra a definição de qualidade (MB/min) vezes a duração do episódio (padrão 45 min).

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
  `{pasta da série}/[Season {N}/]{Título} - S{TT}E{EE}[-E{EE}…] - {Título do episódio} {Qualidade}.{ext}`,
  o formato do gerenciador anterior (multi-episódio no estilo "prefixed range", título de
  episódio de multi-episódio unido por ` + `);
- **título do episódio é obrigatório**: sem ele, a importação espera a próxima volta, e
  importa com `TBA` depois de 48 h da exibição;
- `ffprobe` lê os idiomas; `add_episode_file` grava e liga os episódios;
- o arquivo antigo que ficar sem episódio sai do disco.

Falhas seguem as dos filmes: torrent com erro ou sumido vai para a lista de bloqueio (com
`series_id`) e os episódios são buscados de novo; disco cheio devolve o torrent à fila.
Torrent que o tracker deixou de reconhecer (o avulso apagado quando o pacote sai) conta
como falha de download e leva à busca do pacote.

## Remoção

- **Episódio, temporada ou seleção**: apaga os hardlinks, tira os arquivos do catálogo e
  marca os episódios `deleted`. Arquivo multi-episódio sai inteiro. Depois, cada torrent
  que tinha arquivo com o mesmo inode de um apagado é conferido: se nenhum arquivo dele
  continua com hardlink (`nlink > 1`), ele sai do qBittorrent com os arquivos. Pacote com
  outro episódio ainda na biblioteca fica.
- **Série**: como o filme, com a pasta, os torrents (casamento por inode) e o catálogo.

## Assistidos

Tarefa `assistidos`, junto com os filmes. Episódio assistido por algum usuário do Jellyfin
há mais que a carência, que não é favorito e cuja série não é favorita: o arquivo sai e o
episódio fica `watched`. Arquivo multi-episódio só sai quando todos os episódios dele
foram assistidos. O torrent fica semeando até a limpeza, como nos filmes.

## Migração do gerenciador anterior

Comando único, `series import-sonarr <url> <chave>`, idempotente:

- cada série é achada no TMDB pelo `tvdbId`;
- pasta, `seasonFolder` e `monitorNewItems` vêm de lá;
- episódio monitorado sem arquivo entra sem `skip`, e desmonitorado sem arquivo entra
  `unwanted`;
- cada arquivo entra com caminho, tamanho, qualidade, idiomas, grupo e nome do release;
- episódio do gerenciador que o TMDB não tem (numeração diferente) entra no relatório e
  não no catálogo.
