// vue.config.js
const sass = require('sass-embedded')

module.exports = {
  // with './' the dev server cannot load any arbitrary path
  // with '/' the prod build generates some url(/fonts…) calls in the css chunks, which doesn't work with a servlet context path
  publicPath: process.env.NODE_ENV === 'production' ? './' : '/',
  productionSourceMap: false,

  css: {
    loaderOptions: {
      sass: {
        implementation: sass,
        api: 'modern-compiler',
        sassOptions: {
          loadPaths: [__dirname],
          // Vuetify 2 is frozen on Sass syntax deprecated by current compilers.
          silenceDeprecations: ['slash-div', 'import', 'global-builtin', 'color-functions', 'if-function'],
        },
      },
    },
  },

  chainWebpack: config => {
    // ts-loader already transpiles without checking. Keep type checking out of
    // production builds; lint and unit tests remain explicit quality gates.
    config.plugins.delete('fork-ts-checker')
  },

  pluginOptions: {
    i18n: {
      locale: 'en',
      fallbackLocale: 'en',
      localeDir: 'locales',
      enableInSFC: false,
    },
  },

  devServer: {
    allowedHosts: 'all',
    client: {
      webSocketURL: 'ws://0.0.0.0:8081/ws',
    },
  },

  // custom rule for readium and r2d2bc css that needs to be made available, but untouched
  configureWebpack: {
    module: {
      rules: [
        {
          test: [
            /readium\/.*\.css.resource$/,
            /r2d2bc\/.*\.css.resource$/,
          ],
          type: 'asset/resource',
          generator: {
            filename: 'css/[hash].css[query]',
          },
        },
      ],
    },
  },
}
