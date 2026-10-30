# Obrigação de seed

## O problema

Alguns trackers privados cobram seed de quem baixa. O Amigos (`amigosshare`)
exige de todo download concluído 72 h de seed **ou** ratio 1.0 no torrent, e de
todo torrent que você enviou ao site (um lançamento) 7 dias de seed. A limpeza
automática, sem a tag do modo "construir ratio", apagaria um seed privado sem
hardlink na biblioteca com ratio 1.0, ou 24 h ocioso, ou 120 h de seed, o que
quebraria essa regra.

## A regra

Cada indexador pode declarar o que o tracker exige. Enquanto a obrigação de um
torrent **completo** não foi cumprida, o acervo não o apaga, com ou sem o modo
ratio:

- download: cumprida quando o tempo de seed chega a `seed_min_horas` **ou** a
  ratio chega a `seed_min_ratio` (basta um dos dois; só um definido vale sozinho);
- lançamento: cumprida quando o tempo de seed chega a `seed_envio_horas`.

Torrent incompleto não deve nada: a obrigação começa quando o download conclui.
Indexador sem obrigação configurada não segura nada, e o modo ratio segue como
antes (a tag de semear impede a limpeza por tempo indeterminado).

## De onde vem a obrigação de cada torrent

Lida a cada coleta, do banco (`crates/acervo-hub/src/obligation.rs`):

- grabs de filme e de série, em qualquer estado: obrigação de
  download do indexador do grab;
- torrent irmão que o cross-seed achou: obrigação de download do indexador da
  consulta;
- lançamento enviado (marcado como enviado e não descartado): obrigação de
  envio do indexador de destino, para o `.torrent` gerado, o baixado do site e
  o irmão achado pelo cross-seed do par. Se o mesmo hash também é um download
  (ou aparece em dois indexadores), as obrigações se somam: ele só sai quando
  todas se cumprem.

Se o banco não responde, a coleta falha e a limpeza não roda.

## Caminhos de apagamento cobertos

- limpeza automática (`acervo-janitor`): pula o torrent com obrigação pendente
  (`SkipReason::SeedObligation`);
- importação de filme: grab que perdeu o motivo, arquivos sumidos e torrent sem
  seeds;
- importação de série: arquivos sumidos, sem seeds e sem arquivo útil.
- marcados para apagar (`crates/acervo-hub/src/marks.rs`, `purge_due`): a limpeza
  só apaga o filme, a temporada ou a série marcada quando nenhum torrent com
  hardlink nos arquivos dela deve seed; o resto espera o próximo ciclo.

Nesses três últimos o torrent completo com obrigação pendente fica no cliente,
com aviso no log. O botão "Apagar" da tela Para apagar e os outros apagamentos que você pede na tela não passam por essa regra.

Desligar o modo ratio tira a tag de semear dos torrents do indexador, menos a
dos lançamentos, que continuam marcados.

## A exceção: torrent desregistrado

Torrent que o tracker não reconhece mais não tem como semear, e o tracker não
o conta. Nesse caso o acervo apaga mesmo com obrigação pendente.

## Como configurar

Em Indexadores, no card do indexador, três campos opcionais (vazio: o tracker
não exige):

- Seed mínimo por download (horas), de 1 a 8760;
- Ratio mínima por download, maior que 0 e até 100;
- Seed mínimo do que você lança (horas), de 1 a 8760.

Exemplos:

- Amigos: 72 horas e ratio 1.0 por download; 168 horas (7 dias) no que você
  lança.
- BJ: nenhum campo preenchido. Não há obrigação por torrent; a limpeza segue
  a política padrão (ou a tag do modo ratio).
