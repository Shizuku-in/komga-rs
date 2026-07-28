import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaSyncPointsService from '@/services/komga-syncpoints.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaSyncPoints = new KomgaSyncPointsService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaSyncPoints: KomgaSyncPointsService;
  }
}
