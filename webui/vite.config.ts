import {fileURLToPath, URL} from 'node:url'
import vue from '@vitejs/plugin-vue'
import {defineConfig, loadEnv} from 'vite'
import vuetify, {transformAssetUrls} from 'vite-plugin-vuetify'

const KOMGA_BASE_URL = '__KOMGA_BASE_URL__'

export default defineConfig(({command, mode}) => {
  const env = loadEnv(mode, process.cwd(), '')
  const proxyTarget = env.VITE_KOMGA_PROXY_TARGET || 'http://127.0.0.1:25600'

  return {
    base: './',
    plugins: [
      vue({
        template: {
          transformAssetUrls,
        },
      }),
      vuetify({autoImport: true}),
      {
        name: 'komga-development-base-url',
        transformIndexHtml: command === 'serve'
          ? html => html.replaceAll(KOMGA_BASE_URL, '/')
          : undefined,
      },
    ],
    resolve: {
      alias: {
        '@': fileURLToPath(new URL('./src', import.meta.url)),
      },
    },
    assetsInclude: ['**/*.css.resource'],
    optimizeDeps: {
      entries: ['index.html'],
    },
    server: {
      proxy: {
        '/actuator': proxyTarget,
        '/api': proxyTarget,
        '/login/oauth2': proxyTarget,
        '/oauth2': proxyTarget,
        '/sse': proxyTarget,
      },
    },
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      rollupOptions: {
        output: {
          entryFileNames: 'js/[name]-[hash].js',
          chunkFileNames: 'js/[name]-[hash].js',
          assetFileNames: assetInfo => {
            const name = assetInfo.names?.[0] ?? ''
            if (name.endsWith('.css')) return 'css/[name]-[hash][extname]'
            if (/\.(woff2?|ttf|eot)$/i.test(name)) return 'fonts/[name]-[hash][extname]'
            if (/\.(png|jpe?g|gif|svg|webp|avif)$/i.test(name)) return 'img/[name]-[hash][extname]'
            return 'assets/[name]-[hash][extname]'
          },
        },
      },
    },
    test: {
      environment: 'jsdom',
      globals: true,
      include: ['tests/unit/**/*.spec.ts'],
    },
  }
})
