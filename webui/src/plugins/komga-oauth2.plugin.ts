import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaOauth2Service from '@/services/komga-oauth2.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaOauth2 = new KomgaOauth2Service(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaOauth2: KomgaOauth2Service;
  }
}
