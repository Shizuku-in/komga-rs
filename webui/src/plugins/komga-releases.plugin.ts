import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaAnnouncementsService from '@/services/komga-announcements.service'
import KomgaReleasesService from '@/services/komga-releases.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaReleases = new KomgaReleasesService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaReleases: KomgaReleasesService;
  }
}
