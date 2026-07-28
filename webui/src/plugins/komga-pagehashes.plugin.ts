import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaPageHashesService from '@/services/komga-pagehashes.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaPageHashes = new KomgaPageHashesService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaPageHashes: KomgaPageHashesService;
  }
}
