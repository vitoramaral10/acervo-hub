import type { IndexerStatsDay } from '@/lib/api'
import { formatCount } from '@/lib/format'

/** Rótulo curto do dia: "04/10". */
function dayLabel(day: string) {
  const [, month, date] = day.split('-')
  return `${date}/${month}`
}

/**
 * Barras por dia: a altura é o número de consultas, e a parte de baixo, em
 * vermelho, as que falharam. Cada dia tem dica com os números; a tabela
 * escondida lê o mesmo para leitor de tela.
 */
export function StatsSparkline({ series, label }: { series: IndexerStatsDay[]; label: string }) {
  const max = Math.max(1, ...series.map((day) => day.consultas))
  const slot = 6
  const bar = 4
  const height = 32
  const width = series.length * slot
  const first = series.at(0)
  const last = series.at(-1)
  return (
    <figure className="grid gap-1.5">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        className="h-8 w-full"
        role="img"
        aria-label={`${label}: consultas e falhas por dia`}
      >
        {series.map((day, index) => {
          const total = (day.consultas / max) * height
          const failed = (day.falhas / max) * height
          const x = index * slot + (slot - bar) / 2
          return (
            <g key={day.dia}>
              <title>
                {`${dayLabel(day.dia)}: ${formatCount(day.consultas)} consultas, ${formatCount(day.falhas)} falhas, ${formatCount(day.grabs)} grabs`}
              </title>
              {/* Alvo da dica do tamanho do dia inteiro, não só da barra. */}
              <rect x={index * slot} y={0} width={slot} height={height} fill="transparent" />
              {day.consultas === 0 ? (
                <rect x={x} y={height - 1} width={bar} height={1} className="fill-border-strong" />
              ) : (
                <rect x={x} y={height - total} width={bar} height={total} rx={1} className="fill-accent/70" />
              )}
              {day.falhas > 0 && (
                <rect x={x} y={height - failed} width={bar} height={failed} rx={1} className="fill-danger" />
              )}
            </g>
          )
        })}
      </svg>
      <figcaption className="flex items-center justify-between gap-3 text-xs text-content-subtle">
        <span className="flex items-center gap-3">
          <span className="inline-flex items-center gap-1">
            <span aria-hidden="true" className="size-2 rounded-sm bg-accent/70" />
            Consultas
          </span>
          <span className="inline-flex items-center gap-1">
            <span aria-hidden="true" className="size-2 rounded-sm bg-danger" />
            Falhas
          </span>
        </span>
        {first && last && (
          <span className="tabular-nums">
            {dayLabel(first.dia)} – {dayLabel(last.dia)}
          </span>
        )}
      </figcaption>
      <table className="sr-only">
        <caption>{label} por dia</caption>
        <thead>
          <tr>
            <th scope="col">Dia</th>
            <th scope="col">Consultas</th>
            <th scope="col">Falhas</th>
            <th scope="col">Grabs</th>
          </tr>
        </thead>
        <tbody>
          {series.map((day) => (
            <tr key={day.dia}>
              <th scope="row">{dayLabel(day.dia)}</th>
              <td>{day.consultas}</td>
              <td>{day.falhas}</td>
              <td>{day.grabs}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </figure>
  )
}
