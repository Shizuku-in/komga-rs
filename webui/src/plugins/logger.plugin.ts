import type {App} from 'vue'
import {format} from 'date-fns'

export default {
  install(Vue: App) {
    let isDebug = false
    if (import.meta.env.VITE_LOG_LEVEL || window.localStorage['loglevel'])
      isDebug = true

    Object.defineProperty(Vue.config.globalProperties, '$debug', {
      get: function () {
        // eslint-disable-next-line no-console
        return isDebug ? console.log.bind(console, timestamp(), 'DEBUG ---') : noop
      },
    })

    Object.defineProperty(Vue.config.globalProperties, '$info', {
      get: function () {
        // eslint-disable-next-line no-console
        return isDebug ? console.log.bind(console, timestamp(), 'INFO ---') : noop
      },
    })

    Object.defineProperty(Vue.config.globalProperties, '$warn', {
      get: function () {
        // eslint-disable-next-line no-console
        return isDebug ? console.warn.bind(console, timestamp(), 'WARN ---') : noop
      },
    })

    Object.defineProperty(Vue.config.globalProperties, '$err', {
      get: function () {
        // eslint-disable-next-line no-console
        return isDebug ? console.error.bind(console, timestamp(), 'ERROR ---') : noop
      },
    })
  },
}

function timestamp() {
  return format(new Date(), 'yyyy-MM-dd HH:mm:ss.SSS')
}

function noop() {
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $debug(...msg: any[]): void;
    $info(...msg: any[]): void;
    $warn(...msg: any[]): void;
    $err(...msg: any[]): void;
  }
}
