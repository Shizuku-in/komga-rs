import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaTasksService from '@/services/komga-tasks.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaTasks = new KomgaTasksService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaTasks: KomgaTasksService;
  }
}
