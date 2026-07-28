import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaLoginService from '@/services/komga-login.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaLogin = new KomgaLoginService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaLogin: KomgaLoginService;
  }
}
