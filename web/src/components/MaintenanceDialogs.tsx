import { useQueryClient } from '@tanstack/react-query'
import { Badge } from '@/components/ui/misc'
import { PlanDialog, PlanSection, StepResult } from '@/components/PlanDialog'
import { type MediaKind, type RenameReport, type VerifyReport, library } from '@/lib/api'
import { formatCount, formatSize } from '@/lib/format'

const plural = (n: number, one: string, many: string) => `${formatCount(n)} ${n === 1 ? one : many}`

/** Depois de aplicar, o que a tela mostra mudou: recarrega o que depende do disco. */
function useRefreshLibrary() {
  const queryClient = useQueryClient()
  return () => {
    for (const key of ['filmes', 'series', 'faltando', 'calendario', 'historico', 'fila'])
      void queryClient.invalidateQueries({ queryKey: [key] })
  }
}

const Mono = ({ children }: { children: string }) => (
  <span className="font-mono text-xs break-all">{children}</span>
)

export function RenameDialog({
  kind,
  id,
  title,
  onClose,
}: {
  kind: MediaKind
  id: number
  title: string
  onClose: () => void
}) {
  const refresh = useRefreshLibrary()
  return (
    <PlanDialog<RenameReport>
      title={`Renomear arquivos de ${title}`}
      description="Estes arquivos ganham o nome do padrão da biblioteca. Nada muda até você confirmar."
      run={(aplicar) => library.rename(kind, id, aplicar)}
      isEmpty={(report) => report.plano.length === 0}
      emptyMessage="Tudo já está com o nome certo."
      canApply={() => true}
      applyLabel="Renomear"
      onApplied={refresh}
      onClose={onClose}
      summary={(report) =>
        `${plural(report.renomeados, 'arquivo renomeado', 'arquivos renomeados')}` +
        (report.erros > 0 ? `, ${plural(report.erros, 'erro', 'erros')}` : '')
      }
    >
      {(report) => (
        <PlanSection title="Arquivos" count={report.plano.length}>
          {report.plano.map((step) => (
            <li key={step.arquivo_id} className="grid gap-1 p-3">
              <p className="text-content-subtle">
                <Mono>{step.de}</Mono>
              </p>
              <p className="text-sm">
                <span aria-hidden="true">→ </span>
                <span className="sr-only">para </span>
                <Mono>{step.para}</Mono>
              </p>
              {step.legendas.map((subtitle) => (
                <div key={subtitle.de} className="mt-1 border-l-2 border-border pl-3 text-content-subtle">
                  <p>
                    <Mono>{subtitle.de}</Mono>
                  </p>
                  <p>
                    <span aria-hidden="true">→ </span>
                    <Mono>{subtitle.para}</Mono>
                  </p>
                </div>
              ))}
              <StepResult applied={report.aplicado} done={step.feito} error={step.erro} />
            </li>
          ))}
        </PlanSection>
      )}
    </PlanDialog>
  )
}

const hasChanges = (report: VerifyReport) =>
  report.novos.length + report.sumidos.length + report.legendas.length > 0
const hasContent = (report: VerifyReport) => hasChanges(report) || report.nao_reconhecidos.length > 0

/** As três seções do plano (mais as legendas) de uma verificação. */
function VerifySections({ report }: { report: VerifyReport }) {
  return (
    <>
      {report.novos.length > 0 && (
        <PlanSection title="Novos no disco" count={report.novos.length}>
          {report.novos.map((item) => (
            <li key={item.arquivo} className="grid gap-1 p-3">
              <Mono>{item.arquivo}</Mono>
              <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-content-subtle">
                {item.codigo && <span className="tabular-nums">{item.codigo}</span>}
                {item.qualidade && <span>{item.qualidade}</span>}
                <span className="tabular-nums">{formatSize(item.tamanho)}</span>
                {item.estava_dispensado && <Badge tone="warning">Estava dispensado</Badge>}
              </p>
              <StepResult applied={report.aplicado} done={item.feito} error={item.erro} />
            </li>
          ))}
        </PlanSection>
      )}
      {report.sumidos.length > 0 && (
        <PlanSection title="Sumiram do disco" count={report.sumidos.length}>
          {report.sumidos.map((item) => (
            <li key={item.arquivo_id} className="grid gap-1 p-3">
              <Mono>{item.arquivo}</Mono>
              <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-content-subtle">
                {item.codigo && <span className="tabular-nums">{item.codigo}</span>}
                {item.volta_a_busca && <Badge tone="warning">Volta a ser buscado</Badge>}
              </p>
              <StepResult applied={report.aplicado} done={item.feito} error={item.erro} />
            </li>
          ))}
        </PlanSection>
      )}
      {report.legendas.length > 0 && (
        <PlanSection title="Legendas" count={report.legendas.length}>
          {report.legendas.map((item) => (
            <li key={item.arquivo} className="grid gap-1 p-3">
              <Mono>{item.arquivo}</Mono>
              <p className="text-xs text-content-subtle">
                de <Mono>{item.video}</Mono>
                {item.idioma && ` · ${item.idioma}`}
                {item.forcada && ' · forçada'}
              </p>
              <StepResult applied={report.aplicado} done={item.feito} error={item.erro} />
            </li>
          ))}
        </PlanSection>
      )}
      {report.nao_reconhecidos.length > 0 && (
        <PlanSection title="Não reconhecidos" count={report.nao_reconhecidos.length}>
          {report.nao_reconhecidos.map((item) => (
            <li key={item.arquivo} className="grid gap-1 p-3">
              <Mono>{item.arquivo}</Mono>
              <p className="text-xs text-content-subtle">{item.motivo}</p>
            </li>
          ))}
        </PlanSection>
      )}
    </>
  )
}

const errorsOf = (report: VerifyReport) =>
  [...report.novos, ...report.sumidos, ...report.legendas].filter((item) => item.erro).length
const doneOf = (report: VerifyReport) =>
  [...report.novos, ...report.sumidos, ...report.legendas].filter((item) => item.feito).length

const verifySummary = (done: number, errors: number) =>
  `${plural(done, 'ajuste feito', 'ajustes feitos')}` + (errors > 0 ? `, ${plural(errors, 'erro', 'erros')}` : '')

export function VerifyDialog({
  kind,
  id,
  title,
  onClose,
}: {
  kind: MediaKind
  id: number
  title: string
  onClose: () => void
}) {
  const refresh = useRefreshLibrary()
  return (
    <PlanDialog<VerifyReport>
      title={`Verificar o disco de ${title}`}
      description="Compara a pasta no disco com a biblioteca. Nada muda até você confirmar."
      run={(aplicar) => library.verify(kind, id, aplicar)}
      isEmpty={(report) => !hasContent(report)}
      emptyMessage="O disco já bate com a biblioteca."
      canApply={hasChanges}
      applyLabel="Aplicar"
      onApplied={refresh}
      onClose={onClose}
      summary={(report) => verifySummary(doneOf(report), errorsOf(report))}
    >
      {(report) => <VerifySections report={report} />}
    </PlanDialog>
  )
}

export function VerifyAllMoviesDialog({ onClose }: { onClose: () => void }) {
  const refresh = useRefreshLibrary()
  return (
    <PlanDialog
      title="Verificar todos os filmes"
      description="Compara a pasta de cada filme no disco com a biblioteca. Nada muda até você confirmar."
      run={(aplicar) => library.verifyAllMovies(aplicar)}
      isEmpty={(report) => !report.filmes.some((movie) => hasContent(movie) || movie.aviso)}
      emptyMessage="O disco já bate com a biblioteca."
      canApply={(report) => report.filmes.some(hasChanges)}
      applyLabel="Aplicar em todos"
      onApplied={refresh}
      onClose={onClose}
      summary={(report) =>
        verifySummary(
          report.filmes.reduce((sum, movie) => sum + doneOf(movie), 0),
          report.filmes.reduce((sum, movie) => sum + errorsOf(movie), 0),
        )
      }
    >
      {(report) =>
        report.filmes
          .filter((movie) => hasContent(movie) || movie.aviso)
          .map((movie) => (
            <div key={movie.filme_id} className="grid gap-3 rounded-lg border border-border p-3">
              <h3 className="text-sm font-semibold">{movie.filme}</h3>
              {movie.aviso && <p className="text-xs text-warning">{movie.aviso}</p>}
              <VerifySections report={{ ...movie, aplicado: report.aplicado }} />
            </div>
          ))
      }
    </PlanDialog>
  )
}
