import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { HardDrive } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/misc'
import { type SizeDefinition, library } from '@/lib/api'

type Row = Pick<SizeDefinition, 'qualidade' | 'nome' | 'minimo' | 'preferido' | 'maximo'>

const gb = new Intl.NumberFormat('pt-BR', { minimumFractionDigits: 1, maximumFractionDigits: 1 })

/** MB/min de um trecho vira GB para um filme de 2h (120min). */
function twoHourHint(row: Row): string {
  const min = ((row.minimo ?? 0) * 120) / 1024
  if (!row.maximo) return `≥ ${gb.format(min)} GB`
  const max = (row.maximo * 120) / 1024
  return `${gb.format(min)}–${gb.format(max)} GB`
}

function sameRows(a: Row[], b: Row[]): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

export function SizesSection({ editable }: { editable: boolean }) {
  const queryClient = useQueryClient()
  const sizes = useQuery({ queryKey: ['tamanhos'], queryFn: library.sizes })
  const [rows, setRows] = useState<Row[] | null>(null)

  useEffect(() => {
    if (sizes.data) setRows(sizes.data.tamanhos)
  }, [sizes.data])

  const save = useMutation({
    mutationFn: (value: Row[]) =>
      library.saveSizes(
        value.map(({ qualidade, minimo, preferido, maximo }) => ({ qualidade, minimo, preferido, maximo })),
      ),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['tamanhos'] })
      toast.success('Tamanhos salvos')
    },
    onError: (error: Error) => toast.error(error.message),
  })

  const original = sizes.data?.tamanhos ?? []
  const changed = rows !== null && !sameRows(rows, original)
  const invalid =
    rows?.some((row) => row.maximo != null && row.maximo > 0 && row.minimo != null && row.minimo > row.maximo) ?? false

  function update(index: number, patch: Partial<Row>) {
    setRows((current) => current?.map((row, idx) => (idx === index ? { ...row, ...patch } : row)) ?? current)
  }

  return (
    <section aria-labelledby="tamanhos-titulo" className="max-w-4xl rounded-lg border border-border bg-surface p-6">
      <div>
        <h2 id="tamanhos-titulo" className="flex items-center gap-2 text-lg font-semibold">
          <HardDrive className="size-4.5 text-content-subtle" aria-hidden="true" />
          Tamanhos por qualidade
        </h2>
        <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
          Limites de tamanho em MB por minuto de filme, usados para rejeitar releases fora da faixa e preferir o tamanho
          ideal de cada qualidade.
        </p>
      </div>

      {sizes.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-2">
          {Array.from({ length: 6 }, (_, key) => (
            <Skeleton key={key} className="h-10 w-full" />
          ))}
        </div>
      ) : sizes.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{sizes.error.message}</p>
          <Button onClick={() => void sizes.refetch()}>Tentar novamente</Button>
        </div>
      ) : (rows ?? []).length === 0 ? (
        <div className="mt-6 flex flex-col items-center gap-2 rounded-md border border-dashed border-border-strong px-6 py-10 text-center">
          <HardDrive className="size-6 text-content-subtle" aria-hidden="true" />
          <p className="text-sm text-content-muted">Nenhuma qualidade cadastrada.</p>
        </div>
      ) : (
        <div className="mt-6 overflow-x-auto">
          <table className="w-full min-w-[40rem] border-collapse text-sm">
            <thead>
              <tr className="border-b border-border text-left">
                <th scope="col" className="py-2 pr-3 font-medium">
                  Qualidade
                </th>
                <th scope="col" className="px-3 py-2 font-medium">
                  Mínimo
                  <span className="block text-xs font-normal text-content-subtle">MB/min</span>
                </th>
                <th scope="col" className="px-3 py-2 font-medium">
                  Preferido
                  <span className="block text-xs font-normal text-content-subtle">MB/min</span>
                </th>
                <th scope="col" className="px-3 py-2 font-medium">
                  Máximo
                  <span className="block text-xs font-normal text-content-subtle">MB/min · vazio = ∞</span>
                </th>
                <th scope="col" className="py-2 pl-3 text-right font-medium">
                  ~ filme de 2h
                </th>
              </tr>
            </thead>
            <tbody>
              {(rows ?? []).map((row, index) => {
                const rowInvalid = row.maximo != null && row.maximo > 0 && row.minimo != null && row.minimo > row.maximo
                return (
                  <tr key={row.qualidade} className="border-b border-border last:border-0">
                    <th scope="row" className="py-2 pr-3 text-left font-normal">
                      {row.nome}
                    </th>
                    <td className="px-3 py-2">
                      <Label htmlFor={`tamanho-min-${row.qualidade}`} className="sr-only">
                        Mínimo de {row.nome}
                      </Label>
                      <Input
                        id={`tamanho-min-${row.qualidade}`}
                        type="number"
                        min={0}
                        className="w-24 tabular-nums"
                        value={row.minimo ?? ''}
                        onChange={(event) => update(index, { minimo: Number(event.target.value) || 0 })}
                        disabled={!editable}
                      />
                    </td>
                    <td className="px-3 py-2">
                      <Label htmlFor={`tamanho-pref-${row.qualidade}`} className="sr-only">
                        Preferido de {row.nome}
                      </Label>
                      <Input
                        id={`tamanho-pref-${row.qualidade}`}
                        type="number"
                        min={0}
                        className="w-24 tabular-nums"
                        value={row.preferido ?? ''}
                        onChange={(event) => update(index, { preferido: Number(event.target.value) || 0 })}
                        disabled={!editable}
                      />
                    </td>
                    <td className="px-3 py-2">
                      <Label htmlFor={`tamanho-max-${row.qualidade}`} className="sr-only">
                        Máximo de {row.nome}
                      </Label>
                      <Input
                        id={`tamanho-max-${row.qualidade}`}
                        type="number"
                        min={0}
                        placeholder="∞"
                        title="Vazio ou zero: sem limite"
                        aria-invalid={rowInvalid ? true : undefined}
                        aria-describedby={rowInvalid ? `tamanho-erro-${row.qualidade}` : undefined}
                        className="w-24 tabular-nums"
                        value={row.maximo ?? ''}
                        onChange={(event) => update(index, { maximo: Number(event.target.value) || 0 })}
                        disabled={!editable}
                      />
                      {rowInvalid && (
                        <p id={`tamanho-erro-${row.qualidade}`} role="alert" className="mt-1 text-xs text-danger">
                          Máximo abaixo do mínimo.
                        </p>
                      )}
                    </td>
                    <td className="py-2 pl-3 text-right tabular-nums text-content-muted">{twoHourHint(row)}</td>
                  </tr>
                )
              })}
            </tbody>
          </table>

          {editable && (
            <div className="mt-4 flex justify-end">
              <Button
                variant="primary"
                disabled={!changed || invalid}
                loading={save.isPending}
                onClick={() => rows && save.mutate(rows)}
              >
                Salvar tamanhos
              </Button>
            </div>
          )}
        </div>
      )}
    </section>
  )
}
