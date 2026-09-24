# acervo-hub

Um serviço único, em Rust, para gerenciar uma biblioteca de mídia — no lugar de quatro
processos separados conversando por HTTP.

> **Estado: fase 1, em construção.** Nada aqui é utilizável em produção ainda.

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
| `acervo-arr` | Cliente da API v3: fila, inventário, remoção | existe |
| `acervo-clients` | Clientes de download (hoje qBittorrent) | existe |
| `acervo-hub` | Binário: configuração, coleta, relato, execução | existe |
| `acervo-indexers` | Busca em indexadores (Torznab e Cardigann), rate limit compartilhado | em construção |
| `acervo-metadata` | Provedores de metadados + cache | |
| `acervo-parser` | Parsing de nome de release | |
| `acervo-decision` | Perfis de qualidade, formatos customizados, pontuação | |
| `acervo-library` | Import, hardlink, rename, varredura de filesystem | |
| `acervo-api` | HTTP: superfície nova + compatibilidade com a API v3 existente | Torznab existe |

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

cargo run --bin acervo-hub -- -c config.toml plan    # lê, planeja, relata
cargo run --bin acervo-hub -- -c config.toml apply   # ... e executa
cargo run --bin acervo-hub -- -c config.toml serve   # serve os indexadores
```

`serve` é o outro modo de vida do binário: processo longo que responde Torznab em
`/<indexador>/api`, e em `/all/api` por todos de uma vez. Os gerenciadores de série e de
filme cadastram essa URL como cadastrariam o agregador atual. Três escolhas do desenho:

- **A consulta chega a cada indexador reduzida ao que ele anunciou.** Parâmetro que ele não
  entende é retirado — mas se, sem ele, uma busca por episódio viraria "últimos
  lançamentos", o indexador simplesmente não é consultado.
- **Paginação é local.** Nem todo indexador pagina; pedir a página 2 a quem não pagina
  devolve a 1 de novo, e o consumidor tomaria a repetição por release nova.
- **Falha total não vira lista vazia.** Se todos os indexadores consultados falham, a
  resposta é erro Torznab `900`: "nada encontrado" e "tracker fora do ar" pedem reações
  opostas de quem consulta.

`plan` nunca altera nada, nem grava strikes — repetir a simulação não leva um item ao
limite sem ninguém ter decidido. O modo não é um ramo de código separado: é um campo do
plano, para que o que se valida em seco seja exatamente o que roda de verdade.

Códigos de saída: `0` sucesso, `1` falha de execução, `3` ciclo abortado por trava. O `3`
é próprio para que um agendador distinga "a leitura do mundo não era confiável" de "algo
quebrou".

### Em container

```sh
docker build -t acervo-hub .
```

Imagem final de **~9 MB**: multi-stage com alvo musl, distroless `static` como base,
o binário estático e nada mais. Sem shell, sem gerenciador de pacotes, sem `curl` —
o que não está lá não precisa ser corrigido nem serve a quem entrar.

`deploy/` traz o serviço para um stack Compose existente e as unidades systemd que agendam
o ciclo. Três pontos do desenho que valem atenção:

**O acervo é montado somente leitura, e isso é trava, não zelo.** O janitor só precisa de
`stat(2)` para checar hardlink; toda remoção passa pela API do cliente de download. O
container, portanto, não consegue apagar arquivo do acervo nem se a lógica de decisão
estiver errada.

**O ciclo é one-shot, não daemon.** Fica sob um `profile` do Compose para não subir junto
com o resto do stack; quem dispara é o timer. O padrão do container é `plan` — executar de
verdade exige dizer `apply`.

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
      existentes. Risco baixo, valor imediato. *Falta validar contra instâncias reais.*
- [ ] **Fase 2 — `acervo-indexers`.** Absorve o agregador de indexadores. O cliente
      Torznab, a agregação, o rate limit compartilhado, um executor Cardigann v11
      conservador e a superfície Torznab (`serve`) existem. O executor cobre indexador
      público, GET e HTML UTF-8, e recusa no load qualquer recurso fora desse recorte em
      vez de rodá-lo pela metade. *Esse recorte ainda não carrega nenhuma definição real:*
      contra um corpus de 573, todas são recusadas — as públicas usam settings `select`,
      templates em campo, filtros `re_replace`/`dateparse` e a seção `download`. O teste
      `corpus` (ignorado por padrão) mede isso e ordena o que falta pelo quanto destrava.
      Até lá, `serve` já funciona na frente de endpoints Torznab existentes.
- [ ] **Fase 3 — filmes.** Árvore mais simples; o gerenciador de séries segue de pé como
      controle.
- [ ] **Fase 4 — séries.** Só depois de o parser passar no corpus real.

## Licença

GPL-3.0. Mesma licença dos projetos que este substitui, pela possibilidade de portar
definições de indexador e tabelas de parsing derivadas deles.
