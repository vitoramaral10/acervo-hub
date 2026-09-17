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
| `acervo-indexers` | Busca em indexadores (Torznab/Newznab), rate limit compartilhado | |
| `acervo-metadata` | Provedores de metadados + cache | |
| `acervo-parser` | Parsing de nome de release | |
| `acervo-decision` | Perfis de qualidade, formatos customizados, pontuação | |
| `acervo-library` | Import, hardlink, rename, varredura de filesystem | |
| `acervo-api` | HTTP: superfície nova + compatibilidade com a API v3 existente | |

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
```

`plan` nunca altera nada, nem grava strikes — repetir a simulação não leva um item ao
limite sem ninguém ter decidido. O modo não é um ramo de código separado: é um campo do
plano, para que o que se valida em seco seja exatamente o que roda de verdade.

Códigos de saída: `0` sucesso, `1` falha de execução, `3` ciclo abortado por trava. O `3`
é próprio para que um agendador distinga "a leitura do mundo não era confiável" de "algo
quebrou".

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
- [ ] **Fase 2 — `acervo-indexers`.** Absorve o agregador de indexadores. Torznab é
      contrato fechado.
- [ ] **Fase 3 — filmes.** Árvore mais simples; o gerenciador de séries segue de pé como
      controle.
- [ ] **Fase 4 — séries.** Só depois de o parser passar no corpus real.

## Licença

GPL-3.0. Mesma licença dos projetos que este substitui, pela possibilidade de portar
definições de indexador e tabelas de parsing derivadas deles.
