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

```sh
cp config.example.toml config.toml   # preencha urls, chaves e caminhos
chmod 600 config.toml                # guarda segredo em texto puro

cargo run --bin acervo-hub -- -c config.toml serve   # indexadores, interface e tarefas
cargo run --bin acervo-hub -- -c config.toml apply   # um ciclo de limpeza à mão
cargo run --bin acervo-hub -- -c config.toml sync    # planeja o cadastro nos *arr
cargo run --bin acervo-hub -- -c config.toml sync --apply
cargo run --bin acervo-hub -- -c config.toml search "termo" [-i indexador] [-k 5000]
```

### Interface web

`serve` também serve uma interface em `/`, com o que se fazia pela tela do agregador atual:

- **Indexadores** — estado de cada um (último sucesso, último erro, falhas seguidas), teste,
  ativar e desativar, trocar credencial, remover, e **adicionar** a partir do catálogo de
  definições (`server.catalogs`) ou de qualquer endpoint Torznab. As definições que o
  executor ainda não roda aparecem com o motivo, em vez de sumirem.
- **Busca** manual em todos os indexadores, com download do `.torrent` pela sessão.
- **Filmes** — a biblioteca em pôsteres. Adicionar pelo TMDB; editar monitoramento e
  disponibilidade; buscar os que faltam ou buscar um só, interativamente (cada release com
  qualidade, idiomas e o motivo de cada recusa); apagar o arquivo ou remover o filme — com a
  pasta e o download no qBittorrent, na hora; histórico do filme; cada arquivo conferido
  contra o disco.
- **Atividade** — a fila com o progresso do qBittorrent (tirar da fila, bloquear, buscar
  outro), o histórico de tudo e a lista de bloqueio.
- **Aplicativos** — o `sync` na tela: mostra o que mudaria no gerenciador de séries e aplica.
- **Tarefas** — as rotinas de fundo do serviço (busca dos que faltam, RSS, importação,
  metadados e a limpeza), com intervalo, última e próxima execução e "rodar agora"; e o
  histórico das últimas execuções, gravado no banco — o da limpeza abre o relatório do ciclo.
- **Configurações** — TMDB, regras de decisão (teto de tamanho,
  propers, legenda embutida, carência, prioridade e seeders por indexador, espera, espaço
  livre reservado) e
  notificações (Gotify).

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

Scripts e automação podem, em vez disso, mandar a chave de `server.api_key` no cabeçalho
`X-Api-Key`.

- **A tela não reescreve o `config.toml`**, que pode ficar somente leitura. Indexadores
  adicionados e desativados vão para `[state] registry`; credenciais, para
  `[state] credentials` (gravação atômica, permissão 600). Os dois valem por cima da config.
  Indexador do arquivo não se remove pela tela — desativa-se. Segredo nunca volta para a tela: campo em branco
  mantém o valor atual.
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

A limpeza roda dentro de `serve`, como a tarefa "Limpeza", a cada
`server.cleanup_interval_minutes` (padrão 60). Não há modo de simulação: cada ciclo avança
os strikes e executa o plano, e as travas abortam o ciclo inteiro quando a leitura do mundo
não é confiável.

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

**O diretório de estado precisa ser do UID 65532**, senão os strikes não sobrevivem ao fim
do container e três strikes nunca se completam:

```sh
mkdir -p state && sudo chown 65532:65532 state
```

O `SuccessExitStatus=3` na unidade systemd é proposital: ciclo abortado por trava não é
falha de execução e não deve sujar o status nem disparar alerta.

### Travas que abortam o ciclo inteiro

Nenhuma é ajuste fino. Se uma dispara, a leitura do mundo está errada e **nenhuma** remoção
daquele ciclo é confiável — inclusive as que pareciam corretas:

- instância `*arr` que não respondeu;
- instância que respondeu mas não reporta nenhuma obra (meio-viva é pior que morta);
- biblioteca medindo zero, que desligaria silenciosamente a trava proporcional;
- lote acima do teto absoluto ou da fração do acervo.

## Roteiro

A migração é *strangler*, na ordem do risco. Cada fase é reversível e entrega valor sozinha.

- [x] **Fase 1 — `acervo-janitor`.** Substitui só o faxineiro, falando as APIs v3
      existentes. Risco baixo, valor imediato. Validado contra uma stack real e em produção,
      de hora em hora em modo `apply`, no lugar do faxineiro anterior. A primeira volta
      simulada pegou o que os testes não pegavam: o login do qBittorrent 5.1+ (204 sem
      corpo) e um download manual de 50 GB que seria apagado por não ter hardlink — daí
      `policy.managed_categories`, que restringe essa regra às categorias dos
      gerenciadores.
- [x] **Fase 2 — `acervo-indexers`.** Absorve o agregador de indexadores. Em produção. O cliente
      Torznab, a agregação, o rate limit compartilhado, o executor Cardigann v11 e a
      superfície Torznab (`serve`) existem. O executor roda tracker público e privado —
      login por formulário ou por cookie, sessão refeita quando o site deixa de
      reconhecê-la, download intermediado com a sessão —, com o subconjunto de templates
      Go, filtros e seletores (`:contains` incluído) que as definições reais usam. O que
      ele não cobre é recusado na carga, com o motivo. Contra um corpus de 573 definições,
      carrega 26; as recusas mais comuns são resposta JSON (130), login por `form` (113) e
      a seção `download` (59). O teste `corpus` (ignorado por padrão) refaz essa conta.
      `sync` cadastra os indexadores nos gerenciadores, como a tela de apps do agregador
      atual. Validado contra instâncias reais descartáveis dos dois gerenciadores: ambos
      aceitam o indexador e o feed, e o `sync` cria, mantém e corrige sem apagar ajuste
      manual. Contra um tracker privado real, login por formulário, busca em várias páginas e
      download intermediado funcionam; o login por cookie aguarda um cookie válido para ser
      conferido em produção.
      Consultas iguais são reaproveitadas: pedido idêntico em andamento espera a mesma
      resposta, e a resposta fica guardada por 30 minutos (5 para o feed recente). Os
      gerenciadores e a busca dos filmes que faltam passam pelo mesmo serviço, e cada tracker vê uma
      requisição por consulta. A busca roda até o fim mesmo se o consumidor desistir, e a
      tentativa seguinte a encontra pronta. Em produção desde 24/09/2026 como único
      agregador: o anterior foi desligado.

### Migrando do agregador atual

1. Copie as definições que você usa para `definicoes/` e liste-as em `[[indexers]]`, com
   as credenciais em `settings`.
2. Suba `acervo-hub-indexadores` e rode `sync` — sem `--apply`, ele só mostra o plano.
3. `sync --apply`. Cada gerenciador passa a ter os dois cadastros lado a lado: o antigo e
   o ` (acervo-hub)`.
4. Compare as buscas manuais pelos dois. Satisfeito, desabilite os cadastros antigos no
   gerenciador e pare a sincronização do agregador antes de desligá-lo — senão ele os
   recria.
- [x] **Fase 3 — filmes.** Árvore mais simples; o gerenciador de séries seguiu de pé como
      controle. Em etapas, cada uma conferida contra o gerenciador de filmes em produção
      antes da seguinte:
  - [x] **Parser de release** (`acervo-parser`). Porte do parser de referência: título e
        títulos alternativos, ano, edição, qualidade e revisão, idiomas, grupo, ids
        embutidos. Contra um corpus de 684 títulos reais — o histórico de um gerenciador em
        produção e buscas em trackers, com a leitura dele como gabarito —, concorda em
        todos os campos de todos os títulos, esquisitices incluídas. O corpus fica fora do
        repositório (tem nome de tracker privado); o teste `corpus`, ignorado por padrão,
        refaz a conta.
  - [x] **Catálogo de filmes** (`acervo-store`, Postgres), espelhado do gerenciador de
        filmes enquanto os dois conviveram — 294 filmes e 132 arquivos, todos confirmados no
        disco. `movies check` confere cada arquivo contra o disco.
  - [x] **Motor de decisão** (`acervo-decision`). Porte do da referência: casamento do
        release com o filme (ids do indexador, título limpo, numerais romanos, ano), agregação
        de idiomas ("Original" e nome sem idioma viram o idioma original do filme), as
        especificações de rejeição — perfil, idioma, tamanho por minuto, teto global,
        seeders, disco bruto, legenda embutida, corte, repack, fila — e a ordem de
        preferência. Contra 40 buscas interativas reais (470 releases), com a decisão da
        referência como gabarito: nenhuma divergência em casamento, motivos, ordem ou escolha.
        Por cima disso, o perfil automático (acima) substitui perfis, formatos e tamanhos
        configuráveis.
  - [x] **Grab e import**. `movies grab <tmdb>` (ou "Pegar agora" na tela) busca, decide e
        manda o escolhido ao qBittorrent numa categoria própria (`[movies] category`). O
        serviço confere a cada `import_interval_minutes` os downloads que terminaram e liga o
        maior vídeo na pasta do filme por hardlink, com o nome `Título (ano) {imdb-tt…}`.
  - [x] **Metadados próprios** (`acervo-metadata`, TMDB, chave nas Configurações da tela).
        Datas de cinema, digital e física, status e disponibilidade calculados com as regras
        da referência — contra 293 filmes reais, batem em todos.
  - [x] **API v3 de filmes** para o app de pedidos: status, perfis, pastas raiz, tags,
        filmes (listar, procurar, adicionar, atualizar, remover), fila, comandos e histórico.
  - [x] **Busca automática**, sempre ligada: o RSS de todos os indexadores é
        decidido contra a biblioteca inteira, e os filmes que faltam são buscados em rodadas
        (`search_interval_minutes`, `search_limit`); o botão "Buscar os que faltam" busca todos, em
        segundo plano. A "última busca" de cada filme aparece na tela.
  - [x] **O corte**. O gerenciador de filmes foi desligado: o acervo adotou os filmes com
        os ids dele (que o app de pedidos guarda), e o app de pedidos passou a falar com a API
        v3 daqui. O código que só servia à convivência saiu junto — importação, migração,
        listas de importação e exclusões. Filme entra à mão, pela tela ou pelo app de pedidos.
- [ ] **Fase 4 — séries.** Só depois de o parser passar no corpus real.

## Licença

GPL-3.0. Mesma licença dos projetos que este substitui, pela possibilidade de portar
definições de indexador e tabelas de parsing derivadas deles.
