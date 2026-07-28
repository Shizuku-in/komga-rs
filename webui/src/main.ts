import _, {LoDashStatic} from 'lodash'
import {createApp} from 'vue'
import Chartkick from 'chartkick'
import VueChartkick from 'vue-chartkick'
import 'chartkick/chart.js'
import App from './App.vue'
import actuator from './plugins/actuator.plugin'
import httpPlugin from './plugins/http.plugin'
import komgaBooks from './plugins/komga-books.plugin'
import komgaClaim from './plugins/komga-claim.plugin'
import komgaCollections from './plugins/komga-collections.plugin'
import komgaReadLists from './plugins/komga-readlists.plugin'
import komgaFileSystem from './plugins/komga-filesystem.plugin'
import komgaLibraries from './plugins/komga-libraries.plugin'
import komgaReferential from './plugins/komga-referential.plugin'
import komgaSeries from './plugins/komga-series.plugin'
import komgaUsers from './plugins/komga-users.plugin'
import komgaTransientBooks from './plugins/komga-transientbooks.plugin'
import komgaSse from './plugins/komga-sse.plugin'
import komgaTasks from './plugins/komga-tasks.plugin'
import komgaSyncPoints from './plugins/komga-syncpoints.plugin'
import komgaOauth2 from './plugins/komga-oauth2.plugin'
import komgaLogin from './plugins/komga-login.plugin'
import komgaPageHashes from './plugins/komga-pagehashes.plugin'
import komgaMetrics from './plugins/komga-metrics.plugin'
import komgaHistory from './plugins/komga-history.plugin'
import komgaAnnouncements from './plugins/komga-announcements.plugin'
import komgaReleases from './plugins/komga-releases.plugin'
import komgaSettings from './plugins/komga-settings.plugin'
import komgaFonts from './plugins/komga-fonts.plugin'
import vuetify from './plugins/vuetify'
import logger from './plugins/logger.plugin'
import {createVuetifyCompat} from './plugins/vuetify-compat'
import EventHub from './services/event-hub'
import router from './router'
import store from './store'
import i18n from './i18n'

Chartkick.options = {
  colors: [
    '#7eb0d5', '#fd7f6f', '#b2e061', '#ffb55a',
    '#8bd3c7', '#ffee65', '#bd7ebe', '#fdcce5',
    '#beb9db', '#ea5545', '#f46a9b', '#ef9b20',
    '#edbf33', '#ede15b', '#bdcf32', '#87bc45',
    '#27aeef', '#b33dc6',
  ],
}

const app = createApp(App)
const globals = app.config.globalProperties as any

globals.$_ = _
globals.$eventHub = new EventHub()

app.use(VueChartkick)
app.use(httpPlugin)
app.use(logger)
app.use(komgaSettings, {store, http: globals.$http})
app.use(komgaFileSystem, {http: globals.$http})
app.use(komgaSeries, {http: globals.$http})
app.use(komgaCollections, {http: globals.$http})
app.use(komgaReadLists, {http: globals.$http})
app.use(komgaBooks, {http: globals.$http})
app.use(komgaReferential, {http: globals.$http})
app.use(komgaClaim, {http: globals.$http})
app.use(komgaTransientBooks, {http: globals.$http})
app.use(komgaUsers, {store, http: globals.$http})
app.use(komgaLibraries, {store, http: globals.$http})
app.use(komgaSse, {eventHub: globals.$eventHub, store})
app.use(actuator, {http: globals.$http})
app.use(komgaTasks, {http: globals.$http})
app.use(komgaSyncPoints, {http: globals.$http})
app.use(komgaOauth2, {http: globals.$http})
app.use(komgaLogin, {http: globals.$http})
app.use(komgaPageHashes, {http: globals.$http})
app.use(komgaMetrics, {http: globals.$http})
app.use(komgaHistory, {http: globals.$http})
app.use(komgaAnnouncements, {http: globals.$http})
app.use(komgaReleases, {http: globals.$http})
app.use(komgaFonts, {http: globals.$http})
app.use(router)
app.use(store)
app.use(i18n)
app.use(vuetify)

// vue-i18n 11 folds the Vue 2 `$tc` helper into `$t`. Keep the existing
// templates readable while preserving their choice-based pluralization.
globals.$tc = (key: string, choice: number, named?: Record<string, unknown>) =>
  (i18n.global.t as any)(key, choice, named)

const vuetifyCompat = createVuetifyCompat(vuetify)
globals.$vuetify = vuetifyCompat
app.mixin({
  computed: {
    $vuetify: () => vuetifyCompat,
  },
})

app.mount('#app')

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $_: LoDashStatic;
    $eventHub: EventHub;
    $tc: (key: string, choice: number, named?: Record<string, unknown>) => string;
  }
}

declare global {
  interface Window {
    resourceBaseUrl: string
  }
}
