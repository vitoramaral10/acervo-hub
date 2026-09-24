import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// O build vai para dentro do crate e é embutido no binário com `include_str!`.
// Por isso os nomes são fixos e sem hash, e não há divisão em chunks: o Rust
// precisa saber de antemão cada arquivo que existe.
export default defineConfig({
  base: '/ui/',
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { '@': path.resolve(import.meta.dirname, 'src') },
  },
  build: {
    outDir: '../crates/acervo-api/src/ui/dist',
    emptyOutDir: true,
    assetsInlineLimit: 0,
    cssCodeSplit: false,
    modulePreload: false,
    rollupOptions: {
      output: {
        entryFileNames: 'app.js',
        chunkFileNames: 'app-[name].js',
        assetFileNames: 'app.[ext]',
        codeSplitting: false,
      },
    },
  },
  server: {
    // `npm run dev` fala com um `acervo-hub serve` local.
    proxy: {
      '/ui/api': 'http://127.0.0.1:9797',
      '/ui/baixar': 'http://127.0.0.1:9797',
    },
  },
})
