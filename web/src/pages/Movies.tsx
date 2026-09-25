import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowDownToLine,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CloudDownload,
  ExternalLink,
  Film,
  LoaderCircle,
  Radar,
  SearchX,
} from "lucide-react";
import { type ReactNode, useMemo, useState } from "react";
import { toast } from "sonner";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Badge, Skeleton } from "@/components/ui/misc";
import {
  type GrabReport,
  type Movie,
  type MovieDownload,
  type MovieImport,
  type MovieShadow,
  api,
} from "@/lib/api";
import { formatAgo, formatCount, formatSize } from "@/lib/format";
import { cn } from "@/lib/utils";

type Filter = "todos" | "com-arquivo" | "sem-arquivo" | "problemas";

const FILTERS: { value: Filter; label: string }[] = [
  { value: "todos", label: "Todos" },
  { value: "com-arquivo", label: "No disco" },
  { value: "sem-arquivo", label: "Faltando" },
  { value: "problemas", label: "Com problema" },
];

const DISK = {
  ok: { label: "No disco", tone: "success", icon: CircleCheck },
  missing: { label: "Ausente do disco", tone: "danger", icon: CircleAlert },
  size_differs: {
    label: "Tamanho diferente",
    tone: "warning",
    icon: CircleAlert,
  },
  unreadable: { label: "Não deu para ler", tone: "warning", icon: CircleAlert },
} as const;

/** Motivos de rejeição, na voz da tela. */
const REASONS: Record<string, string> = {
  UnknownMovie: "de outro filme",
  WrongMovie: "de outro filme",
  UnableToParse: "nome ilegível",
  QualityNotWanted: "qualidade fora do perfil",
  WantedLanguage: "sem o idioma original",
  BelowMinimumSize: "pequeno demais",
  AboveMaximumSize: "grande demais",
  MaximumSizeExceeded: "acima do teto",
  MinimumSeeders: "poucos seeders",
  MinimumFreeSpace: "disco sem espaço",
  Raw: "disco bruto",
  HardcodeSubtitles: "legenda embutida",
  Sample: "amostra",
  QueueCutoffMet: "já baixando",
  QueueHigherPreference: "já baixando",
  QueueUpgradesNotAllowed: "já baixando",
};

function ShadowLine({ shadow }: { shadow: MovieShadow }) {
  const when = formatAgo(shadow.quando);
  if (shadow.erro) {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-danger">
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Sombra {when}: a busca falhou — {shadow.erro}
        </span>
      </p>
    );
  }
  if (shadow.pegaria) {
    return (
      <p
        className="mt-1 flex items-center gap-1.5 text-xs text-accent"
        title={shadow.pegaria}
      >
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Sombra {when}: pegaria{" "}
          <span className="font-mono">{shadow.pegaria}</span>
        </span>
      </p>
    );
  }
  const reasons =
    shadow.releases === 0
      ? "nenhum resultado"
      : shadow.motivos
          .map(([reason, count]) => `${REASONS[reason] ?? reason} (${count})`)
          .join(", ");
  return (
    <p className="mt-1 flex items-center gap-1.5 text-xs text-content-subtle">
      <Radar className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="truncate">
        Sombra {when}: nada entre {formatCount(shadow.releases)} — {reasons}
      </span>
    </p>
  );
}

function DownloadLine({ download }: { download: MovieDownload }) {
  const when = formatAgo(download.pego_em);
  if (download.estado === "failed") {
    return (
      <p
        className="mt-1 flex items-center gap-1.5 text-xs text-danger"
        title={download.release}
      >
        <CircleAlert className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Download {when} falhou
          {download.mensagem ? ` — ${download.mensagem}` : ""}
        </span>
      </p>
    );
  }
  if (download.estado === "imported") {
    return (
      <p
        className="mt-1 flex items-center gap-1.5 text-xs text-success"
        title={download.release}
      >
        <CircleCheck className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Importado — o Radarr adota o arquivo ao reler a pasta
        </span>
      </p>
    );
  }
  return (
    <p
      className="mt-1 flex items-center gap-1.5 text-xs text-accent"
      title={download.release}
    >
      <LoaderCircle
        className="size-3.5 shrink-0 animate-spin motion-reduce:animate-none"
        aria-hidden="true"
      />
      <span className="truncate">
        Baixando{download.mensagem ? ` (${download.mensagem})` : ""}:{" "}
        <span className="font-mono">{download.release}</span>
      </span>
    </p>
  );
}

/** Busca, mostra a escolha e só pega depois de confirmar. */
function GrabDialog({
  movie,
  open,
  onOpenChange,
}: {
  movie: Movie;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [plan, setPlan] = useState<GrabReport | null>(null);
  const search = useMutation({
    mutationFn: () => api.grab(movie.id, false),
    onSuccess: setPlan,
  });
  const take = useMutation({
    mutationFn: () => api.grab(movie.id, true),
    onSuccess: (report) => {
      if (report.aplicado && report.escolhido) {
        toast.success(`${report.filme}: mandado ao qBittorrent`);
        onOpenChange(false);
      } else {
        // A escolha mudou entre a busca e a confirmação.
        setPlan(report);
      }
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () =>
      void queryClient.invalidateQueries({ queryKey: ["filmes"] }),
  });
  const pick = plan?.escolhido;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (next) {
          setPlan(null);
          search.mutate();
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            Pegar {movie.titulo}
            {movie.ano ? ` (${movie.ano})` : ""}
          </DialogTitle>
          <DialogDescription>
            Busca em todos os indexadores e escolhe com as regras do Radarr.
            Nada é baixado antes de você confirmar.
          </DialogDescription>
        </DialogHeader>
        <div className="min-h-24" aria-live="polite">
          {search.isPending ? (
            <div className="grid gap-2" aria-busy="true">
              <Skeleton className="h-4 w-3/4" />
              <Skeleton className="h-4 w-1/2" />
              <p className="text-xs text-content-subtle">
                Buscando nos indexadores…
              </p>
            </div>
          ) : search.isError ? (
            <p role="alert" className="text-sm text-danger">
              {search.error.message}
            </p>
          ) : pick && plan ? (
            <dl className="grid gap-3 rounded-md border border-border p-4 text-sm">
              <div>
                <dt className="text-xs text-content-subtle">Release</dt>
                <dd className="font-mono text-xs break-all">{pick.titulo}</dd>
              </div>
              <div className="flex flex-wrap gap-x-6 gap-y-2">
                <div>
                  <dt className="text-xs text-content-subtle">Indexador</dt>
                  <dd>{pick.indexador}</dd>
                </div>
                <div>
                  <dt className="text-xs text-content-subtle">Qualidade</dt>
                  <dd>{pick.qualidade}</dd>
                </div>
                <div>
                  <dt className="text-xs text-content-subtle">Tamanho</dt>
                  <dd className="tabular-nums">{formatSize(pick.tamanho)}</dd>
                </div>
              </div>
              <p className="text-xs text-content-subtle">
                Melhor de {formatCount(plan.releases)} releases.
              </p>
            </dl>
          ) : plan ? (
            <p className="text-sm text-content-muted">
              Nenhum release serve entre {formatCount(plan.releases)}
              {plan.motivos.length > 0 &&
                ` — ${plan.motivos.map(([reason, count]) => `${REASONS[reason] ?? reason} (${count})`).join(", ")}`}
              .
            </p>
          ) : null}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancelar
          </Button>
          <Button
            variant="primary"
            disabled={!pick}
            loading={take.isPending}
            onClick={() => take.mutate()}
          >
            {!take.isPending && <ArrowDownToLine aria-hidden="true" />}
            Pegar este
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

const hasProblem = (movie: Movie) =>
  movie.arquivo !== null && movie.arquivo.disco !== "ok";

function normalize(text: string) {
  return text
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLowerCase();
}

function summarize(instances: MovieImport[]) {
  const failed = instances.find((instance) => instance.erro);
  if (failed) return { ok: false, text: `${failed.nome}: ${failed.erro}` };
  const total = instances.reduce(
    (sum, instance) => {
      const r = instance.resumo;
      if (!r) return sum;
      return {
        created: sum.created + r.created.length,
        updated: sum.updated + r.updated.length,
        removed: sum.removed + r.removed.length,
      };
    },
    { created: 0, updated: 0, removed: 0 },
  );
  if (total.created + total.updated + total.removed === 0)
    return { ok: true, text: "Catálogo já estava em dia" };
  return {
    ok: true,
    text: `${total.created} novos, ${total.updated} atualizados, ${total.removed} removidos`,
  };
}

export function MoviesPage() {
  const queryClient = useQueryClient();
  const movies = useQuery({ queryKey: ["filmes"], queryFn: api.movies });
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("todos");
  const [selected, setSelected] = useState<number | null>(null);

  const importer = useMutation({
    mutationFn: () => api.importMovies(true),
    onSuccess: ({ instancias }) => {
      if (instancias.length === 0) {
        toast.error(
          "Nenhum gerenciador de filmes configurado em [[instances]]",
        );
        return;
      }
      const result = summarize(instancias);
      if (result.ok) toast.success(result.text);
      else toast.error(result.text);
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () =>
      void queryClient.invalidateQueries({ queryKey: ["filmes"] }),
  });

  const shadow = useMutation({
    mutationFn: () => api.shadow(5),
    onSuccess: ({ filmes }) =>
      toast.success(`Sombra: ${filmes.length} filmes buscados`),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () =>
      void queryClient.invalidateQueries({ queryKey: ["filmes"] }),
  });

  const list = movies.data?.filmes;
  const counts = useMemo(() => {
    const all = list ?? [];
    return {
      total: all.length,
      withFile: all.filter((m) => m.arquivo).length,
      problems: all.filter(hasProblem).length,
      size: all.reduce((sum, m) => sum + (m.arquivo?.tamanho ?? 0), 0),
    };
  }, [list]);

  const visible = useMemo(() => {
    const needle = normalize(query.trim());
    return (list ?? [])
      .filter((movie) => {
        if (filter === "com-arquivo" && !movie.arquivo) return false;
        if (filter === "sem-arquivo" && movie.arquivo) return false;
        if (filter === "problemas" && !hasProblem(movie)) return false;
        if (!needle) return true;
        const haystack = normalize(
          [
            movie.titulo,
            movie.titulo_original ?? "",
            String(movie.ano ?? ""),
            movie.imdb ?? "",
          ].join(" "),
        );
        return haystack.includes(needle);
      })
      .sort((a, b) => sortKey(a).localeCompare(sortKey(b), "pt-BR"));
  }, [list, query, filter]);

  const current = list?.find((movie) => movie.id === selected) ?? null;
  const filters = FILTERS.filter(
    (f) => f.value !== "problemas" || counts.problems > 0,
  );

  return (
    <>
      <PageHeader
        title="Filmes"
        description={
          movies.isSuccess
            ? `${formatCount(counts.total)} filmes, ${formatCount(counts.withFile)} no disco (${formatSize(counts.size)}).`
            : "Sua biblioteca de filmes."
        }
        action={
          <div className="flex flex-wrap gap-2">
            <Button
              variant="ghost"
              onClick={() => shadow.mutate()}
              loading={shadow.isPending}
              disabled={counts.total === 0}
            >
              {!shadow.isPending && <Radar aria-hidden="true" />}
              Sombra
            </Button>
            <Button
              variant="ghost"
              onClick={() => importer.mutate()}
              loading={importer.isPending}
            >
              {!importer.isPending && <CloudDownload aria-hidden="true" />}
              Importar do Radarr
            </Button>
          </div>
        }
      />

      {movies.isPending ? (
        <LibrarySkeleton />
      ) : movies.isError ? (
        <div
          role="alert"
          className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6"
        >
          <p className="font-medium">Não foi possível ler o catálogo.</p>
          <p className="text-sm text-content-muted">{movies.error.message}</p>
          <Button onClick={() => void movies.refetch()}>
            Tentar novamente
          </Button>
        </div>
      ) : counts.total === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <Film className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">A biblioteca está vazia</p>
          <p className="max-w-md text-sm text-content-muted">
            Importe os filmes do Radarr. Nada muda nele: o acervo-hub só lê a
            lista, os perfis e os arquivos.
          </p>
          <Button
            variant="primary"
            onClick={() => importer.mutate()}
            loading={importer.isPending}
          >
            Importar do Radarr
          </Button>
        </div>
      ) : (
        <>
          <div className="mb-6 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <label className="sm:w-80">
              <span className="sr-only">Buscar na biblioteca</span>
              <Input
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Buscar por título, ano ou IMDb"
              />
            </label>
            <div
              role="radiogroup"
              aria-label="Filtro"
              className="flex flex-wrap rounded-md border border-border p-0.5"
            >
              {filters.map(({ value, label }) => (
                <button
                  key={value}
                  type="button"
                  role="radio"
                  aria-checked={filter === value}
                  onClick={() => setFilter(value)}
                  className={cn(
                    "rounded-sm px-2.5 py-1.5 text-xs font-medium text-content-muted transition-colors hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring",
                    filter === value && "bg-surface-raised text-content",
                  )}
                >
                  {label}
                  {value === "problemas" && (
                    <span className="ml-1 text-danger tabular-nums">
                      {counts.problems}
                    </span>
                  )}
                </button>
              ))}
            </div>
          </div>

          {visible.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border-strong px-6 py-12 text-center">
              <SearchX
                className="size-6 text-content-subtle"
                aria-hidden="true"
              />
              <p className="font-medium">Nenhum filme com esse filtro</p>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setQuery("");
                  setFilter("todos");
                }}
              >
                Limpar filtros
              </Button>
            </div>
          ) : (
            <ul className={GRID} aria-label="Filmes">
              {visible.map((movie) => (
                <li key={movie.id}>
                  <PosterCard
                    movie={movie}
                    onOpen={() => setSelected(movie.id)}
                  />
                </li>
              ))}
            </ul>
          )}
          {visible.length !== counts.total && (
            <p className="mt-4 text-xs text-content-subtle tabular-nums">
              {formatCount(visible.length)} de {formatCount(counts.total)}
            </p>
          )}
        </>
      )}

      {current && (
        <MovieDetails
          movie={current}
          open={selected !== null}
          onOpenChange={(open) => !open && setSelected(null)}
        />
      )}
    </>
  );
}

const GRID =
  "grid grid-cols-[repeat(auto-fill,minmax(6.5rem,1fr))] gap-x-3 gap-y-5 sm:gap-x-4 sm:gap-y-6 sm:grid-cols-[repeat(auto-fill,minmax(10rem,1fr))]";

/** Ordem de estante: sem artigo inicial. */
function sortKey(movie: Movie) {
  return normalize(movie.titulo).replace(/^(the|a|an|o|os|as|um|uma)\s+/, "");
}

const STATUS: Record<string, string> = {
  announced: "Anunciado",
  inCinemas: "No cinema",
  released: "Lançado",
};

function Poster({ movie, className }: { movie: Movie; className?: string }) {
  const [failed, setFailed] = useState(false);
  return (
    <div
      className={cn(
        "relative aspect-[2/3] overflow-hidden rounded-md border border-border bg-surface-raised",
        className,
      )}
    >
      {movie.poster && !failed ? (
        <img
          src={movie.poster}
          alt=""
          loading="lazy"
          decoding="async"
          onError={() => setFailed(true)}
          className="size-full object-cover"
        />
      ) : (
        <div className="flex size-full flex-col items-center justify-center gap-2 p-3 text-center">
          <Film className="size-6 text-content-subtle" aria-hidden="true" />
          <span className="line-clamp-3 text-xs text-content-muted">
            {movie.titulo}
          </span>
        </div>
      )}
    </div>
  );
}

/** O que falta dizer sobre o filme na capa; filme no disco não precisa de marca. */
function PosterMark({ movie }: { movie: Movie }) {
  if (hasProblem(movie)) {
    const disk = DISK[movie.arquivo!.disco];
    return (
      <Badge tone={disk.tone} className="shadow-sm">
        <disk.icon aria-hidden="true" />
        {disk.label}
      </Badge>
    );
  }
  if (movie.arquivo) return null;
  if (movie.download?.estado === "downloading") {
    return (
      <Badge tone="accent" className="shadow-sm">
        <LoaderCircle
          className="animate-spin motion-reduce:animate-none"
          aria-hidden="true"
        />
        Baixando
      </Badge>
    );
  }
  if (movie.download?.estado === "failed") {
    return (
      <Badge tone="danger" className="shadow-sm">
        <CircleAlert aria-hidden="true" />
        Falhou
      </Badge>
    );
  }
  return (
    <Badge className="shadow-sm">
      <CircleDashed aria-hidden="true" />
      {movie.monitorado ? "Faltando" : "Não monitorado"}
    </Badge>
  );
}

function PosterCard({ movie, onOpen }: { movie: Movie; onOpen: () => void }) {
  const missing = !movie.arquivo;
  return (
    <button
      type="button"
      onClick={onOpen}
      className="group block w-full rounded-md text-left focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring"
    >
      <div className="relative">
        <Poster
          movie={movie}
          className={cn(
            "transition-[transform,opacity] duration-150 ease-out group-hover:-translate-y-0.5 group-hover:border-border-strong motion-reduce:transform-none",
            missing && "opacity-60 group-hover:opacity-100",
          )}
        />
        <div className="absolute top-2 left-2 max-w-[calc(100%-1rem)]">
          <PosterMark movie={movie} />
        </div>
      </div>
      <p className="mt-2 line-clamp-2 text-sm leading-snug font-medium">
        {movie.titulo}
      </p>
      <p className="mt-0.5 text-xs text-content-subtle tabular-nums">
        {movie.ano ?? "—"}
        {movie.arquivo && ` · ${movie.arquivo.qualidade}`}
      </p>
    </button>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <dt className="text-xs text-content-subtle">{label}</dt>
      <dd className="mt-0.5 text-sm">{children}</dd>
    </div>
  );
}

function MovieDetails({
  movie,
  open,
  onOpenChange,
}: {
  movie: Movie;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [grabbing, setGrabbing] = useState(false);
  const file = movie.arquivo;
  const disk = file ? DISK[file.disco] : null;
  const imdb = movie.imdb ? `https://www.imdb.com/title/${movie.imdb}/` : null;
  const canGrab = !file && movie.download?.estado !== "downloading";
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl gap-0 p-0">
        <div className="grid gap-6 p-6 sm:grid-cols-[12rem_1fr]">
          <Poster movie={movie} className="mx-auto w-40 sm:w-full" />
          <div className="flex min-w-0 flex-col gap-4">
            <DialogHeader>
              <DialogTitle className="text-2xl">
                {movie.titulo}{" "}
                {movie.ano && (
                  <span className="font-normal text-content-subtle tabular-nums">
                    ({movie.ano})
                  </span>
                )}
              </DialogTitle>
              {movie.titulo_original &&
                movie.titulo_original !== movie.titulo && (
                  <p className="text-sm text-content-subtle">
                    {movie.titulo_original}
                  </p>
                )}
            </DialogHeader>
            <DialogDescription className="max-w-[65ch] leading-relaxed">
              {movie.sinopse || "Sem sinopse."}
            </DialogDescription>

            <dl className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-3">
              {file ? (
                <>
                  <Fact label="Qualidade">{file.qualidade}</Fact>
                  <Fact label="Tamanho">
                    <span className="tabular-nums">
                      {formatSize(file.tamanho)}
                    </span>
                  </Fact>
                  {file.idiomas.length > 0 && (
                    <Fact label="Áudio">{file.idiomas.join(", ")}</Fact>
                  )}
                </>
              ) : (
                <Fact label="Arquivo">
                  {movie.monitorado ? "Faltando" : "Não monitorado"}
                </Fact>
              )}
              {movie.status && (
                <Fact label="Lançamento">
                  {STATUS[movie.status] ?? movie.status}
                </Fact>
              )}
              {movie.perfil && <Fact label="Perfil">{movie.perfil}</Fact>}
              {disk && file && (
                <Fact label="Disco">
                  <span
                    className={cn(
                      "inline-flex items-center gap-1",
                      disk.tone === "success"
                        ? "text-success"
                        : disk.tone === "danger"
                          ? "text-danger"
                          : "text-warning",
                    )}
                  >
                    <disk.icon className="size-3.5" aria-hidden="true" />
                    {disk.label}
                  </span>
                </Fact>
              )}
            </dl>

            {(file?.release || movie.download || movie.sombra) && (
              <div className="border-t border-border pt-3">
                {file?.release && (
                  <p className="font-mono text-xs break-all text-content-subtle">
                    {file.release}
                  </p>
                )}
                {file?.disco_detalhe && (
                  <p className="mt-1 text-xs text-warning">
                    {file.disco_detalhe}
                  </p>
                )}
                {!file && movie.download ? (
                  <DownloadLine download={movie.download} />
                ) : (
                  !file && movie.sombra && <ShadowLine shadow={movie.sombra} />
                )}
              </div>
            )}
          </div>
        </div>
        <DialogFooter className="border-t border-border px-6 py-4">
          {imdb && (
            <Button asChild variant="ghost">
              <a href={imdb} target="_blank" rel="noreferrer">
                IMDb
                <ExternalLink aria-hidden="true" />
              </a>
            </Button>
          )}
          {canGrab && (
            <Button variant="primary" onClick={() => setGrabbing(true)}>
              <ArrowDownToLine aria-hidden="true" />
              Pegar agora
            </Button>
          )}
        </DialogFooter>
        {canGrab && (
          <GrabDialog
            movie={movie}
            open={grabbing}
            onOpenChange={setGrabbing}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function LibrarySkeleton() {
  return (
    <div aria-busy="true" aria-label="Carregando filmes">
      <Skeleton className="mb-6 h-9 w-80 max-w-full" />
      <div className={GRID}>
        {Array.from({ length: 18 }, (_, key) => (
          <div key={key}>
            <Skeleton className="aspect-[2/3] rounded-md" />
            <Skeleton className="mt-2 h-4 w-3/4" />
            <Skeleton className="mt-1 h-3 w-1/3" />
          </div>
        ))}
      </div>
    </div>
  );
}
