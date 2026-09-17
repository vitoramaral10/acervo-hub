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

| Crate | Responsabilidade |
|---|---|
| `acervo-core` | Domínio puro, zero IO: `Work`, `Item`, `Release`, `Decision` |
| `acervo-janitor` | Reconciliação: órfãos de fila, hardlink perdido, limpeza de download |
| `acervo-indexers` | Busca em indexadores (Torznab/Newznab), rate limit compartilhado |
| `acervo-metadata` | Provedores de metadados + cache |
| `acervo-parser` | Parsing de nome de release |
| `acervo-decision` | Perfis de qualidade, formatos customizados, pontuação |
| `acervo-clients` | Clientes de download (trait `DownloadClient`) |
| `acervo-library` | Import, hardlink, rename, varredura de filesystem |
| `acervo-api` | HTTP: superfície nova + compatibilidade com a API v3 existente |

### Duas decisões que mandam no projeto

**Compatibilidade de API não é opcional.** Gerenciadores de legenda e portais de pedido
falam a API v3 dos serviços existentes. Substituir os quatro sem expor um subconjunto
compatível (`/series`, `/movie`, `/queue`, `/history`, `/qualityprofile`, `/rootfolder` e
webhooks) quebra o resto do ecossistema. Por isso `acervo-api` nasce com duas superfícies.

**O parser é o crate perigoso.** O parsing de nome de release é uma década de regex
acumulada contra a criatividade dos grupos de scene. É tabela de dados, não lógica — mas
validar um port exige corpus real. Quando o parser erra, não há crash: há import silencioso
no lugar errado.

## Roteiro

A migração é *strangler*, na ordem do risco. Cada fase é reversível e entrega valor sozinha.

- [ ] **Fase 1 — `acervo-janitor`.** Substitui só o faxineiro, falando as APIs v3
      existentes. Risco baixo, valor imediato.
- [ ] **Fase 2 — `acervo-indexers`.** Absorve o agregador de indexadores. Torznab é
      contrato fechado.
- [ ] **Fase 3 — filmes.** Árvore mais simples; o gerenciador de séries segue de pé como
      controle.
- [ ] **Fase 4 — séries.** Só depois de o parser passar no corpus real.

## Licença

GPL-3.0. Mesma licença dos projetos que este substitui, pela possibilidade de portar
definições de indexador e tabelas de parsing derivadas deles.
