# acervo-hub

Um serviço único, em Rust, que cuida de uma biblioteca de filmes e séries: busca nos
indexadores, decide o release, manda ao cliente de torrent, importa o que terminou, mantém
os metadados, sugere apagar o que já foi assistido e limpa o que sobrou no cliente — no lugar dos
quatro processos separados (gerenciador de séries, gerenciador de filmes, agregador de
indexadores e faxineiro) que a stack usual roda conversando por HTTP.

> **Estado: em produção — filmes, séries, indexadores, limpeza e "Para apagar".**

Dois princípios mandam no produto:

- **Tudo roda de verdade.** Não há modo de simulação. Cada tarefa age; o que protege o
  acervo são as travas, que abortam a ação quando a leitura do mundo não é confiável.
- **Configura-se só o necessário.** Não há perfil de qualidade, tabela de tamanhos nem
  disponibilidade mínima para editar: o que tem uma resposta certa vem pronto.

## Por quê

Os quatro serviços da stack usual são, na prática, **um serviço só, particionado por
acidente histórico**: o gerenciador de séries e o de filmes são o mesmo código com um
discriminador de tipo; o agregador existe para os dois dividirem rate limit de tracker; o
faxineiro faz, por HTTP, um *join* entre bancos que costumam morar no mesmo Postgres.

O custo dessa partição é uma classe de falha. Quando uma instância trava, o faxineiro
estoura o timeout **antes** de avaliar qualquer coisa, e o ciclo inteiro morre sem limpar
nada. Em processo único, "download sem dono" é uma consulta ao próprio catálogo.

## Arquitetura

Workspace Cargo, binário único `acervo-hub`:

| Crate | Responsabilidade |
|---|---|
| `acervo-core` | Domínio puro, sem IO: downloads, fila, inventário, tamanhos |
| `acervo-parser` | Nome de release: título, ano, temporada e episódio, qualidade, idiomas, grupo |
| `acervo-decision` | Casamento com o filme ou a série, rejeições e ordem de preferência |
| `acervo-indexers` | Busca em indexadores (Cardigann e Torznab), rate limit, proxy e `FlareSolverr` |
| `acervo-api` | O catálogo de indexadores servido, com cache de consultas, e a interface web |
| `acervo-metadata` | Metadados do TMDB |
| `acervo-clients` | qBittorrent e Jellyfin |
| `acervo-fs` | Tradução de caminho container→host e `stat(2)` |
| `acervo-janitor` | A limpeza: decide o que apagar do cliente, sem executar nada |
| `acervo-store` | Catálogo, fila, histórico, configuração e contas no Postgres |
| `acervo-hub` | Binário: tarefas de fundo, importação, decisão aplicada, interface |

O acervo não expõe API para terceiros. A superfície HTTP é a interface e a API JSON dela;
script e automação podem usá-la com a chave de API do servidor no cabeçalho `X-Api-Key`.

**O parser é o crate perigoso.** O parsing de nome de release é uma década de regex
acumulada contra a criatividade dos grupos de scene: tabela de dados, não lógica — e
validar exige corpus real. Quando o parser erra, não há crash: há import silencioso no
lugar errado. Os corpora de filmes e de séries rodam como testes ignorados, com o caminho
do JSON no ambiente.

## Como rodar

Toda a configuração mora no Postgres e se edita na tela, sem reiniciar. Fora do banco há
só duas variáveis de ambiente:

| Variável | Para quê | Padrão |
|---|---|---|
| `ACERVO_DATABASE_URL` | `postgres://usuário:senha@host:5432/banco` — obrigatória | — |
| `ACERVO_BIND` | Endereço de escuta do `serve` | `0.0.0.0:9797` |

```sh
export ACERVO_DATABASE_URL=postgres://acervo:senha@localhost:5432/acervo

cargo run --bin acervo-hub -- serve                       # o serviço
echo 'senha-longa' | cargo run --bin acervo-hub -- users set admin
cargo run --bin acervo-hub -- users list
cargo run --bin acervo-hub -- users remove admin
```

A linha de comando tem só isso: `serve` e as contas da interface. A senha vem da entrada
padrão, nunca de argumento. As tabelas são criadas e migradas na primeira conexão; banco
novo sobe com os padrões, e o resto se preenche em **Configurações**.

## O que o serviço faz

### Descobrir

Lançamentos de filmes no Brasil por semana ISO, além das listas do TMDB de **Em alta**,
**Populares**, **Em breve** (filmes) e **No ar** (séries). Obras que já estão no catálogo
não aparecem nas listas. A busca unificada encontra filmes e séries, incluindo títulos
ocultos ou já no acervo. Ao abrir um título, aparecem sinopse, elenco, direção ou criação,
trailer e recomendações, com adição ao acervo ou acesso à obra já cadastrada.
É possível ocultar títulos, semanas e gêneros, e mostrá-los de novo.
A chave do TMDB é configurada em **Configurações → TMDB**.

### Busca e decisão

Filmes e séries entram pela tela, a partir do TMDB. Todo filme e toda série usam o **perfil
automático**: da melhor qualidade de arquivo para a pior (Remux 2160p … SD), sem upgrade.
Os seeders pesam antes — um release com 5 ou mais vence qualquer um mais fraco, e a
qualidade decide dentro da faixa. O tamanho por minuto de cada qualidade é uma tabela fixa.

- **Busca dos que faltam** (`busca`): os prioritários primeiro, depois os há mais tempo sem
  busca. Agendada, pega um lote; pelo botão, todos.
- **RSS** (`rss`): os releases recentes de todos os indexadores, casados com a biblioteca
  inteira.
- **Busca interativa**: cada release com qualidade, idiomas e o motivo de cada recusa; o
  escolhido à mão pula a decisão.

Um filme tem um download por vez: com um em andamento, o grab é recusado ("já há um
download em andamento"). A exceção é a troca na fila — o release melhor que aparece
enquanto o anterior ainda espera, sem ter começado, toma o lugar dele.

### Fila por espaço

O grab manda o torrent ao qBittorrent **parado**, na categoria do acervo e com a tag
`acervo:fila`. A fila inicia o que cabe no disco, do menor para o maior, pulando quem não
cabe, com os prioritários antes, sem passar do limite de downloads simultâneos e sempre
deixando a folga mínima livre. O espaço livre não basta: desconta-se o que falta baixar de
todo download ativo, porque o cliente reserva aos poucos.

Torrent com a tag da fila, na categoria do acervo, que nenhum grab em andamento reclama
não é de ninguém: a fila o apaga com os arquivos, e nunca o inicia — a menos que algum
arquivo dele tenha outro hardlink.

### Importação

A tarefa `importacao` (a cada 5 minutos) liga por hardlink o que terminou na pasta do
filme ou da série, com o nome do padrão; lê os idiomas com `ffprobe`; leva as legendas do
torrent junto; e pede ao Jellyfin, se configurado, que varra a biblioteca. Depois, uma vez
por volta, roda a fila.

O que o cliente diz uma vez só não derruba o download:

- **Arquivos sumidos** (`missingFiles`) e **erro do cliente com espaço livre** tentam de
  novo até persistirem 30 minutos observados pelo processo. Na primeira vez que o cliente
  diz `missingFiles`, o acervo manda verificar o torrent de novo, e o mesmo torrent baixa o
  que falta. Só se persistir 30 minutos depois disso o arquivo sumido vira falha sem
  bloqueio, e o torrent sai do cliente (se nenhum arquivo tem outro link); o erro, falha com
  bloqueio de 7 dias.
- **Terminado, mas fora do disco**: o cliente dá o torrent por completo e o vídeo não está
  lá. O acervo manda verificar o torrent de novo e espera; se o arquivo continuar sumido
  depois disso, fica "em atenção".
- **Disco cheio** devolve o torrent à fila, com o que já baixou.
- **Sem seeds**: além do que o cliente diz, o torrent precisa estar ativo e sem seed há 30
  minutos observados; torrent que sai de parado ou da fila recomeça a contagem.
- **Desregistrado no tracker** em duas voltas seguidas é falha, e numa série a nova busca
  prefere o pacote.

Falha de download bloqueia o release (os bloqueios automáticos expiram em 7 dias) e busca
de novo. Filme que ganhou arquivo por outro caminho desiste do grab, sem bloquear. Problema
na importação em si (arquivo no caminho, discos diferentes) fica "em atenção", tentando a
cada volta; 6 horas seguidas assim avisam pelo Gotify, uma vez por grab.

### Metadados, cena e definições

- `metadados` atualiza filmes (a cada dia) e séries (a cada 12 h) pelo TMDB, gravando só o
  que vem da base: monitorado, pasta, prioridade e as escolhas da tela ficam como estão.
- `cena` baixa a numeração de cena (XEM) uma vez por dia.
- `definicoes` baixa as definições Cardigann do repositório oficial uma vez por dia e troca
  a de cada indexador em uso que mudou, sem reiniciar.

### Para apagar

Assistido não sai sozinho. Na tela **Para apagar** fica o que o usuário marcou — um filme,
uma temporada ou a série inteira, pelo botão "Marcar para apagar" no detalhe de cada um —,
com o espaço que cada item libera e o total. Nada sai antes de confirmar ali: apagar os
selecionados ou todos pede confirmação com o total. O filme sai como no "Remover" com
arquivos (catálogo, pasta e download); a temporada e a série, como no "Apagar temporada"
(os arquivos e os torrents que ficam sem uso; a série continua no catálogo, com os
episódios apagados fora da busca). Falha num item não segura os outros, e a marca só some
do que saiu.

Com o Jellyfin configurado, a mesma tela mostra **sugestões**, lidas na hora: o filme que
algum usuário assistiu há mais que a carência (padrão 60 minutos) e que ninguém marcou
como favorito, e a temporada em que todos os episódios no disco passam nessa regra. Só
entra o arquivo que chegou antes de assistirem: o Jellyfin lembra o assistido de um título
apagado, e o mesmo título adicionado de novo seria sugerido assim que importado. Assistido
sem data, ou arquivo sem data de adição, fica de fora. Cada sugestão se marca num clique,
ou todas de uma vez; marcar não apaga.

### Limpeza

`limpeza` (a cada 60 minutos) lê a fila do acervo, o cliente e o disco, e apaga do cliente:

- o **seed que perdeu o hardlink** com a biblioteca — o privado só depois do ratio alvo, da
  ociosidade ou do teto de tempo de seed;
- o **download sem dono** — sem grab em andamento, sem seed e sem hardlink — depois de
  aparecer assim em ciclos seguidos.

Só nas categorias gerenciadas. As travas abortam o ciclo inteiro quando a leitura não é
confiável: fila do acervo ilegível, catálogo vazio, lote maior que o teto absoluto ou que a
fração da biblioteca, biblioteca que mede zero. Uma vez por dia, a limpeza também poda o
banco: as buscas com mais de 30 dias (a mais recente de cada obra fica) e os bloqueios
automáticos vencidos. O histórico fica.

### Conferência do disco

`disco` (a cada 6 horas) confere no disco cada arquivo que o catálogo diz estar na
biblioteca, de filmes e de séries. O que sumiu (só "arquivo não existe" conta; outro erro de
leitura deixa o arquivo como está) volta pelo **mesmo torrent**, se ele ainda está no
cliente: o grab importado que o trouxe volta a "baixando", o registro do arquivo sai do
catálogo, e o torrent volta à fila do acervo e é verificado de novo — a fila o inicia quando
couber, e a importação liga o arquivo como da primeira vez. Sem o torrent, o registro só sai
do catálogo, e a busca dos que faltam o pega.

Num pacote de temporada, só voltam os episódios cujo arquivo sumiu: o grab passa a querer só
esses, e os outros arquivos do torrent — episódio que está no disco, assistido ou dispensado
— ficam com prioridade zero.

A conferência nunca apaga torrent nem arquivo. Raiz da biblioteca ausente é disco
desmontado: nada é feito naquele tipo. Na hora agendada, sumiço em massa (mais da metade dos
arquivos, e ao menos 5) também não é reparado, com o aviso no resumo; **rodar agora**
confirma o reparo.

### Tarefas

Cada tarefa tem intervalo editável na tela **Tarefas** (vale na hora; zero desliga o
agendamento), "rodar agora" e o histórico das últimas execuções, com o relatório de cada
uma. Tarefa sem o que precisa — cliente de download — fica parada, com o motivo.
Execução que passa de 2 horas é dada como falha e a tarefa segue agendada; cada consulta ao
banco tem teto de 60 segundos.

Na subida, as tarefas que começam na hora largam espalhadas, com um atraso aleatório de 0 a
90 segundos cada, e a `importacao` só começa depois de o qBittorrent responder. No SIGTERM o
serviço para de agendar execuções novas e espera as que estão rodando por até 25 segundos
antes de sair (o compose dá 30 s de `stop_grace_period`).

### Saúde e avisos

- `GET /health` é a vivacidade: responde `ok` se o processo está de pé.
- `GET /health/ready`, sem login, é a prontidão: confere o banco, o qBittorrent e o espaço
  livre contra a folga das regras. Responde 200 com o JSON do que passou, ou 503 com o que
  falhou, sem endereço nem credencial. Sem cliente de download configurado, essas duas
  verificações são puladas.
- `acervo-hub healthcheck` faz esse GET na porta de `ACERVO_BIND` e sai com 0 ou 1: é o
  `healthcheck` do `deploy/compose.yaml`, já que a imagem não tem curl.

Além dos avisos por filme, o Gotify recebe avisos operacionais, **só na transição** — quando
o estado vira ruim e quando volta, nunca a cada rodada. Cada um tem a própria chave em
Configurações → Notificações, ligada por padrão:

| Chave | Avisa quando |
| --- | --- |
| `tarefa_falhando` | uma tarefa falha 3 vezes seguidas, e quando volta a passar |
| `indexador_falhando` | um indexador falha 5 vezes seguidas, e quando volta a responder |
| `disco_baixo` | o espaço livre da pasta de download cai abaixo de 5% do disco ou da folga das regras (o que for maior), e quando volta |
| `limpeza_abortada` | uma trava de segurança aborta o ciclo de limpeza, e quando deixa de abortar |

O estado dos avisos fica em memória: reiniciar o serviço o zera.

**Disjuntor de indexador.** Depois de 5 falhas seguidas de qualquer tipo — timeout,
Cloudflare, login recusado —, o indexador sai das buscas e dos downloads por 5 minutos; se a
primeira consulta depois disso também falha, a espera dobra, até 6 horas. Um sucesso zera
tudo. O "testar" da tela fura a espera, para conferir uma credencial consertada. A tela de
indexadores mostra "em espera até" (também para o 429). Em memória: reiniciar zera.

## Interface

`serve` responde em `/`:

- **Descobrir** — a tela de entrada: lançamentos semanais, listas e busca unificada do
  TMDB, com detalhes, elenco, trailer, recomendações, adição ao acervo e preferências para
  ocultar títulos, semanas e gêneros.
- **Filmes** e **Séries** — a biblioteca em pôsteres: adicionar pelo TMDB, monitorar,
  prioridade, busca automática ou interativa, renomear, verificar o disco, apagar arquivo
  ou remover — com a pasta e o download no cliente, na hora.
- **Faltando**, **Para apagar** (o que foi marcado, com o espaço que libera, e as
  sugestões de assistidos), **Calendário**, **Atividade** (a fila com o progresso do
  cliente, o histórico e a lista de bloqueio) e **Busca** manual em todos os indexadores.
- **Indexadores** — estado e estatística de cada um, teste, ativar e desativar, trocar
  credencial, remover e adicionar a partir do catálogo de definições ou de um endpoint
  Torznab. As definições que o executor ainda não roda aparecem com o motivo.
- **Configurações** — cliente de download, Jellyfin, biblioteca, limpeza, regras de
  decisão, notificações (Gotify), TMDB e servidor (chave de API, rede dos indexadores,
  catálogo de definições).

Segurança:

- **Segredo nunca volta para a tela.** Senhas e chaves chegam como "definida" ou não;
  salvar com o campo em branco mantém o valor guardado. Nada disso vai para o log.
- **Senha em argon2id, sessão em cookie `HttpOnly` e `SameSite=Strict`.** O banco guarda
  só o SHA-256 do token da sessão, que vale 30 dias; trocar a senha derruba todas as
  sessões. Toda ação que muda estado exige um cabeçalho que formulário de outra origem não
  consegue mandar. A CSP só aceita script servido pelo próprio binário.

O front é React + TypeScript + Tailwind, em `web/`. O build é versionado em
`crates/acervo-api/src/ui/dist` e embutido no binário: `cargo build` e a imagem não
precisam de Node.

```sh
npm ci --prefix web
npm run --prefix web dev      # Vite em :5173, falando com um serve em 127.0.0.1:9797
npm run --prefix web build    # atualiza o build embutido
```

## Em container

```sh
docker build -t acervo-hub .
```

Multi-stage com alvo musl e distroless `static` como base: o binário estático e um
`ffprobe` estático, que lê os idiomas das faixas do que o acervo importa. Sem shell, sem
gerenciador de pacotes, sem `curl`.

`deploy/compose.yaml` traz o serviço para um stack Compose existente. Não há diretório de
estado: configuração, indexadores, fila e strikes moram no Postgres, e o container roda com
o sistema de arquivos somente leitura, montando a biblioteca (com escrita: a importação cria
os hardlinks) e, se houver, as definições Cardigann próprias. A garantia de não apagar
arquivo da biblioteca fica no código: a limpeza só age pela API do cliente, e só sobre o
que não tem hardlink.
