# Telas em tempo real

As telas se atualizam sozinhas quando algo muda no acervo, sem F5 e sem
polling curto. E a sessão do Cloudflare Access que expira deixa de virar
"Sem conexão com o acervo-hub".

## Canal de eventos (SSE)

- `GET /ui/api/eventos` (`text/event-stream`), com a mesma autenticação das
  outras rotas da interface (sessão; `X-Api-Key` também vale para teste).
  Cabeçalhos: `Cache-Control: no-cache`, `X-Accel-Buffering: no`, e os de
  segurança da interface.
- Mensagem: `event: mudanca` com `data: {"topicos":["fila","filmes"]}`.
  Comentário de keep-alive (`: ping`) a cada 20 s: o túnel e o Access fecham
  conexão ociosa.
- Ao conectar, manda `event: ola` (o cliente usa para saber que está vivo).
- Barramento: `tokio::sync::broadcast` com capacidade folgada (ex. 256) no
  estado compartilhado (`Web`, e quem mais emitir: tarefas, job de
  lançamento). Assinante lento que perde mensagens (`Lagged`) recebe um
  `mudanca` com todos os tópicos (recarrega tudo) em vez de cair.
- Mensagens juntadas: mudanças no mesmo tópico dentro de 500 ms viram uma
  só (debounce no servidor), para um lote de 42 pacotes não disparar 42
  recargas por segundo.

## Tópicos

`fila`, `filmes`, `series`, `lancar`, `tarefas`, `indexadores`,
`configuracoes`, `apagar`, `descobrir`, `historico`.

Quem emite:
- **Toda requisição de interface que muda algo** (método diferente de GET,
  resposta 2xx): um middleware deriva os tópicos do caminho (prefixo da rota
  → tópicos; na dúvida, o tópico da tela da rota) e emite depois da
  resposta.
- **Tarefas**: ao terminar cada execução, por id: `importacao` → `fila`,
  `filmes`, `series`, `historico`; `busca`/`rss` → `fila`, `historico`;
  `limpeza` → `fila`; `cross-seed` → `lancar`; `disco` → `filmes`,
  `series`; `metadados`/`cena` → `filmes`, `series`; e sempre `tarefas`
  (início e fim).
- **Job de lançamento**: a cada mudança de estado ou de andamento do pacote
  → `lancar`.
- **Fila de downloads**: a importação já roda a cada 5 min; além disso, a
  tela Fila mantém o polling de progresso enquanto há download ativo (o
  andamento em % não vira evento).

## Cliente

- Um `EventSource('/ui/api/eventos')` para o app inteiro (hook em `App.tsx`),
  aberto só com sessão. Cada tópico invalida as `queryKey` do react-query
  daquela tela (mapa único, junto do cliente de API).
- Reconexão: a do próprio `EventSource`; ao reconectar (`ola` depois de um
  erro), invalida tudo, porque eventos podem ter sido perdidos.
- Polling curto que existir só para "ver se mudou" sai; fica o de progresso
  (download ativo, pacote em preparo) com intervalo maior, já que o evento
  avisa a mudança de estado.
- Indicador discreto no cabeçalho: ponto verde "ao vivo" / cinza
  "reconectando".

## Sessão do Access expirada

- `request()` usa `redirect: 'manual'`. Resposta `opaqueredirect` (ou
  redirecionamento para fora da origem) = o Access quer revalidar a sessão:
  a página recarrega sozinha (o navegador refaz o login do Access
  normalmente). Proteção contra laço: no máximo uma recarga automática a cada
  60 s (`sessionStorage`); depois disso mostra "Sua sessão expirou —
  recarregue a página" com botão.
- O `EventSource` que falha repetidas vezes (3 erros seguidos) faz uma
  chamada de teste com `request()`, que cai na regra acima.
