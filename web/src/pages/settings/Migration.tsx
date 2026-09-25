import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowDownToLine, TriangleAlert } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { library } from '@/lib/api'
import { formatCount } from '@/lib/format'

export function MigrationSection({ hasRadarr }: { hasRadarr: boolean }) {
  const queryClient = useQueryClient()

  const migrate = useMutation({
    mutationFn: () => library.migrate(),
    onSuccess: () => {
      toast.success('Migração concluída')
      void queryClient.invalidateQueries({ queryKey: ['exclusoes'] })
      void queryClient.invalidateQueries({ queryKey: ['listas'] })
      void queryClient.invalidateQueries({ queryKey: ['notificacoes'] })
    },
    onError: (err: Error) => toast.error(err.message),
  })

  if (!hasRadarr) return null

  const report = migrate.data

  return (
    <section aria-labelledby="migracao-titulo" className="max-w-2xl rounded-lg border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="migracao-titulo" className="text-lg font-semibold">
            Trazer do Radarr
          </h2>
          <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
            Copia histórico, bloqueados, exclusões, a notificação do Gotify e as listas de importação do Radarr. Segura
            de rodar mais de uma vez: histórico já migrado é pulado.
          </p>
        </div>
        <Button variant="primary" loading={migrate.isPending} onClick={() => migrate.mutate()}>
          {!migrate.isPending && <ArrowDownToLine aria-hidden="true" />}
          Migrar agora
        </Button>
      </div>

      {report && (
        <dl className="mt-6 grid gap-3 border-t border-border pt-4 text-sm sm:grid-cols-2">
          <div>
            <dt className="text-xs text-content-subtle">Histórico</dt>
            <dd className="tabular-nums">
              {report.historico_ja_migrado ? 'Já migrado' : `${formatCount(report.historico)} eventos`}
            </dd>
          </div>
          <div>
            <dt className="text-xs text-content-subtle">Bloqueados</dt>
            <dd className="tabular-nums">{formatCount(report.bloqueados)}</dd>
          </div>
          <div>
            <dt className="text-xs text-content-subtle">Exclusões novas</dt>
            <dd className="tabular-nums">{formatCount(report.exclusoes)}</dd>
          </div>
          <div>
            <dt className="text-xs text-content-subtle">Notificação</dt>
            <dd>{report.notificacao ? 'Copiada' : 'Não copiada'}</dd>
          </div>
          <div className="sm:col-span-2">
            <dt className="text-xs text-content-subtle">Listas</dt>
            <dd>{report.listas.length > 0 ? report.listas.join(', ') : 'Nenhuma'}</dd>
          </div>
          {report.avisos.length > 0 && (
            <div className="sm:col-span-2">
              <dt className="text-xs text-content-subtle">Avisos</dt>
              <dd className="grid gap-1">
                {report.avisos.map((aviso, index) => (
                  <p key={index} className="flex items-start gap-1.5 text-warning">
                    <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
                    <span>{aviso}</span>
                  </p>
                ))}
              </dd>
            </div>
          )}
        </dl>
      )}
    </section>
  )
}
