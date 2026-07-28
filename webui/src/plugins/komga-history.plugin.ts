import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaHistoryService from '@/services/komga-history.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaHistory = new KomgaHistoryService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaHistory: KomgaHistoryService;
  }
}
