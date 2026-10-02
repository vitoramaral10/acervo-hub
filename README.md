# acervo-hub

Um serviço único, em Rust, para gerenciar uma biblioteca de mídia — no lugar de quatro
processos separados conversando por HTTP.

> **Estado: indexadores, limpeza e filmes em produção; séries ainda no gerenciador de séries.**

## Por quê

A stack usual de automação de mídia roda como quatro serviços independentes: um gerenciador
de séries, um de filmes, um agregador de indexadores e um faxineiro que reconcilia o que os
outros três deixaram para trás. Na prática eles são **um serviço só, particionado por
acidente histórico**:

- O gerenciador de séries e o de filmes são o mesmo código com um discriminador de tipo de
  mídia. Um filme é uma série de um episódio só.
- O agregador de indexadores existe porque os dois primeiros são processos separados e
  precisam de alguém para compartilhar rate limit de tracker.
- O faxineiro faz, por HTTP, um *join* entre bancos que frequentemente já moram no mesmo
  Postgres.

O custo dessa partição não é memória — é uma classe de falha. Quando uma instância trava, o
faxineiro estoura o timeout do cliente HTTP **antes** de avaliar qualquer coisa, e o ciclo
inteiro morre sem limpar nada. O sintoma chega como "não apagou X"; a causa é um healthcheck
vermelho três serviços adiante.

Em processo único, com uma transação, "item de fila sem obra correspondente" deixa de ser um
cruzamento de três APIs e vira:

```sql
select q.id, q.download_id
from   queue_item q
left   join work w on w.id = q.work_id
where  w.id is null;
```

## Arquitetura

Workspace Cargo, binário único `acervo-hub`:

| Crate | Responsabilidade | Estado |
|---|---|---|
| `acervo-core` | Domínio puro, zero IO: `Work`, `Item`, `Download`, `Inventory` | existe |
| `acervo-janitor` | Reconciliação: órfãos de fila, hardlink perdido, limpeza de download | existe |
| `acervo-fs` | Tradução de caminho container→host e `stat(2)` | existe |
| `acervo-arr` | Cliente da API v3 do gerenciador de séries: fila, inventário, indexadores | existe |
| `acervo-clients` | Clientes de download (hoje qBittorrent) | existe |
| `acervo-hub` | Binário: configuração, coleta, relato, execução | existe |
| `acervo-indexers` | Busca em indexadores (Torznab e Cardigann), rate limit compartilhado | existe |
| `acervo-metadata` | Metadados do TMDB | existe |
| `acervo-parser` | Parsing de nome de release | existe |
| `acervo-decision` | Casamento com o filme, rejeições e ordem de preferência (perfil automático) | existe |
| `acervo-store` | Catálogo, histórico, fila e contas no Postgres | existe |
| `acervo-api` | HTTP: Torznab, interface web e compatibilidade com a API v3 de filmes | existe |

### Duas decisões que mandam no projeto

**Compatibilidade de API não é opcional.** Gerenciadores de legenda e portais de pedido
falam a API v3 dos serviços existentes. Substituir os quatro sem expor um subconjunto
compatível (`/series`, `/movie`, `/queue`, `/history`, `/qualityprofile`, `/rootfolder` e
webhooks) quebra o resto do ecossistema. Por isso `acervo-api` nasce com duas superfícies.

**O parser é o crate perigoso.** O parsing de nome de release é uma década de regex
acumulada contra a criatividade dos grupos de scene. É tabela de dados, não lógica — mas
validar um port exige corpus real. Quando o parser erra, não há crash: há import silencioso
no lugar errado.

## Como rodar

Toda a configuração mora no Postgres e se edita na tela, sem reiniciar. Fora do banco há
só duas variáveis de ambiente:

| Variável | Para quê | Padrão |
|---|---|---|
| `ACERVO_DATABASE_URL` | `postgres://usuário:senha@host:5432/banco` — obrigatória para todo comando | — |
| `ACERVO_BIND` | Endereço de escuta do `serve` | `0.0.0.0:9797` |

```sh
export ACERVO_DATABASE_URL=postgres://acervo:senha@localhost:5432/acervo

cargo run --bin acervo-hub -- serve   # indexadores, interface e tarefas
cargo run --bin acervo-hub -- apply   # um ciclo de limpeza à mão
cargo run --bin acervo-hub -- sync    # planeja o cadastro nos *arr
cargo run --bin acervo-hub -- sync --apply
cargo run --bin acervo-hub -- search "termo" [-i indexador] [-k 5000]
```

As tabelas são criadas na primeira conexão. Banco novo sobe com os padrões; o resto se
preenche em **Configurações**: Cliente de download, Jellyfin, Biblioteca, Limpeza, Regras de
decisão, Notificações, TMDB e Servidor (onde se gera a chave de API). Os gerenciadores
(Sonarr, Radarr) se cadastram em **Aplicativos**; os intervalos das tarefas, na própria tela
**Tarefas**. Sem chave de API definida, Torznab e API v3 recusam tudo.

**Vindo do `config.toml`?** Uma vez só, aponte `ACERVO_IMPORT_CONFIG` para o arquivo antigo
e suba o `serve`: com o banco ainda sem configuração, ele lê o arquivo e os arquivos de
`[state]` (credenciais, indexadores adicionados/desativados/removidos e strikes) e grava tudo
numa transação, logando quantas seções, indexadores e strikes entraram. Com o banco já
configurado, a variável é ignorada — tire-a do ambiente depois. `[database]` e
`server.bind` do arquivo são ignorados: vêm das variáveis acima.

### Interface web

`serve` também serve uma interface em `/`, com o que se fazia pela tela do agregador atual:

- **Indexadores** — estado de cada um (último sucesso, último erro, falhas seguidas), teste,
  ativar e desativar, trocar credencial, remover, e **adicionar** a partir do catálogo de
  definições (diretórios em Configurações → Servidor) ou de qualquer endpoint Torznab. As definições que o
  executor ainda não roda aparecem com o motivo, em vez de sumirem.
- **Busca** manual em todos os indexadores, com download do `.torrent` pela sessão.
- **Filmes** — a biblioteca em pôsteres. Adicionar pelo TMDB; editar monitoramento e
  disponibilidade; buscar os que faltam ou buscar um só, interativamente (cada release com
  qualidade, idiomas e o motivo de cada recusa); apagar o arquivo ou remover o filme — com a
  pasta e o download no qBittorrent, na hora; histórico do filme; cada arquivo conferido
  contra o disco.
- **Atividade** — a fila com o progresso do qBittorrent (tirar da fila, bloquear, buscar
  outro), o histórico de tudo e a lista de bloqueio.
- **Aplicativos** — os gerenciadores (adicionar, editar, remover) e o `sync` na tela: mostra
  o que mudaria em cada um e aplica.
- **Tarefas** — as rotinas de fundo do serviço (busca dos que faltam, RSS, importação,
  metadados, a limpeza e, com o Jellyfin configurado, apagar assistidos), com intervalo
  editável (vale na hora; tarefa sem o que precisa fica parada, com o motivo), última e
  próxima execução e "rodar agora"; e o histórico das últimas execuções, gravado no banco —
  o da limpeza abre o relatório do ciclo, o dos assistidos a lista do que saiu e do que ficou,
  o dos metadados o que mudou e qual filme falhou, com o motivo.
- **Configurações** — um item de menu por seção: cliente de download, Jellyfin, biblioteca
  (pastas, caminhos, categoria), limpeza (strikes, carências, travas), regras de decisão,
  notificações (Gotify), TMDB e servidor (chave de API, endereço público, catálogo de
  definições, timeout HTTP).

A qualidade não se configura: todo filme usa o **perfil automático**, da melhor qualidade de
arquivo para a pior (Remux 2160p … SD), sem upgrade. Os seeders pesam antes: um release com
5 ou mais vence qualquer um mais fraco, e a qualidade decide dentro da faixa. Espaço livre
não trava o grab — com o disco cheio, o cliente pausa o download.

Entra-se com usuário e senha, cadastrados por linha de comando — a senha vem da entrada
padrão, nunca de argumento:

```sh
echo 'senha-longa' | acervo-hub users set admin   # cria ou troca a senha
acervo-hub users list
acervo-hub users remove admin
```

Scripts e automação podem, em vez disso, mandar a chave de API do servidor no cabeçalho
`X-Api-Key`.

- **Segredo nunca volta para a tela.** Senhas e chaves (do cliente, do Jellyfin, dos
  gerenciadores, dos indexadores e a do servidor) chegam como "definida" ou não; salvar com o
  campo em branco mantém o valor guardado. Nada disso vai para o log. Indexadores são todos
  cadastros do banco: editáveis e removíveis.
- **Senha em argon2id, sessão em cookie `HttpOnly` e `SameSite=Strict`.** O banco guarda
  só o SHA-256 do token da sessão, que vale 30 dias; sair apaga a sessão no servidor, e
  trocar a senha derruba todas as abertas. Toda ação que muda estado exige
  um cabeçalho que formulário de outra origem não consegue mandar. A CSP só aceita script
  servido pelo próprio binário.
- O front é React + TypeScript + Tailwind, em `web/`. O build é versionado em
  `crates/acervo-api/src/ui/dist` e embutido no binário: `cargo build` e a imagem não
  precisam de Node. Mudou o front? `npm run --prefix web build` — o CI confere.

```sh
npm ci --prefix web
npm run --prefix web dev      # Vite em :5173, falando com um serve em 127.0.0.1:9797
npm run --prefix web build    # atualiza o build embutido
```

`serve` é o outro modo de vida do binário: processo longo que responde Torznab em
`/<indexador>/api`, e em `/all/api` por todos de uma vez. Os gerenciadores de série e de
filme cadastram essa URL como cadastrariam o agregador atual. Três escolhas do desenho:

- **A consulta chega a cada indexador reduzida ao que ele anunciou.** Parâmetro que ele não
  entende é retirado — mas se, sem ele, uma busca por episódio viraria "últimos
  lançamentos", o indexador simplesmente não é consultado.
- **Paginação é local.** Nem todo indexador pagina; pedir a página 2 a quem não pagina
  devolve a 1 de novo, e o consumidor tomaria a repetição por release nova.
- **O cadastro nos gerenciadores é declarativo.** `sync` compara o que cada instância tem
  com o que deveria ter e cria, atualiza ou remove — só os indexadores com o sufixo
  ` (acervo-hub)`. Os do agregador atual ficam intocados, então os dois convivem durante a
  migração. Habilitações, prioridade e tags ajustadas na interface são preservadas.
- **Falha total não vira lista vazia.** Se todos os indexadores consultados falham, a
  resposta é erro Torznab `900`: "nada encontrado" e "tracker fora do ar" pedem reações
  opostas de quem consulta.

A limpeza roda dentro de `serve`, como a tarefa "Limpeza", a cada 60 minutos por padrão
(editável em Tarefas). Não há modo de simulação: cada ciclo avança
os strikes e executa o plano, e as travas abortam o ciclo inteiro quando a leitura do mundo
não é confiável.

Com o Jellyfin configurado, a tarefa "Apagar assistidos" roda a cada 15 minutos por padrão:
o filme que algum usuário do Jellyfin assistiu, há mais que a carência (padrão 60 minutos)
e que ninguém marcou como favorito, sai do catálogo com a pasta e o download — sem
simulação. Assistido sem data conhecida fica.

Códigos de saída: `0` sucesso, `1` falha de execução, `3` ciclo abortado por trava. O `3`
é próprio para que um agendador distinga "a leitura do mundo não era confiável" de "algo
quebrou".

### Em container

```sh
docker build -t acervo-hub .
```

Multi-stage com alvo musl e distroless `static` como base: o binário estático e um
`ffprobe` estático (≈135 MB, a maior parte da imagem), que lê as faixas de áudio e legenda
do que o acervo importa. Sem shell, sem gerenciador de pacotes, sem `curl` — o que não está
lá não precisa ser corrigido nem serve a quem entrar.

`deploy/` traz o serviço para um stack Compose existente. Dois pontos do desenho que
valem atenção:

**O ciclo roda dentro do serviço**, agendado com as outras tarefas, e só remove pela API do
cliente de download: o acervo é montado com escrita porque a importação cria o hardlink do
filme, então a garantia de não apagar arquivo da biblioteca fica no código do janitor.

**Não há diretório de estado.** Configuração, cadastros de indexador e strikes moram no
Postgres; o container pode rodar com o sistema de arquivos somente leitura, montando só o
acervo e, se houver, as definições Cardigann.
