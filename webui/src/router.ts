import urls from '@/functions/urls'
import {createRouter, createWebHistory} from 'vue-router'
import store from './store'
import {LIBRARIES_ALL, LIBRARY_ROUTE} from '@/types/library'
import qs from 'qs'

const lStore = store as any

const adminGuard = (to: any, from: any, next: any) => {
  if (!lStore.getters.meAdmin) next({name: 'home'})
  else next()
}

const noLibraryGuard = (to: any, from: any, next: any) => {
  if (lStore.state.komgaLibraries.libraries.length === 0) {
    next({name: 'welcome'})
  } else next()
}

const noLibraryNorPinGuard = (to: any, from: any, next: any) => {
  if (lStore.state.komgaLibraries.libraries.length === 0) {
    next({name: 'welcome'})
  } else if (lStore.getters.getLibrariesPinned.length === 0) {
    next({name: 'no-pins'})
  } else next()
}

const getLibraryRoute = (libraryId: string) => {
  switch ((lStore.getters.getLibraryRoute(libraryId) as LIBRARY_ROUTE)) {
    case LIBRARY_ROUTE.COLLECTIONS:
      return 'browse-collections'
    case LIBRARY_ROUTE.READLISTS:
      return 'browse-readlists'
    case LIBRARY_ROUTE.BROWSE:
      return 'browse-libraries'
    case LIBRARY_ROUTE.BOOKS:
      return 'browse-books'
    case LIBRARY_ROUTE.RECOMMENDED:
    default:
      return libraryId === LIBRARIES_ALL ? 'browse-libraries' : 'recommended-libraries'
  }
}

const router = createRouter({
  history: createWebHistory(urls.base),
  parseQuery(query: string) {
    return qs.parse(query)
  },
  stringifyQuery(query: Object) {
    const res = qs.stringify(query)
    return res
  },
  routes: [
    {
      path: '/',
      name: 'home',
      redirect: {name: 'dashboard'},
      component: () => import( './views/HomeView.vue'),
      children: [
        {
          path: '/welcome',
          name: 'welcome',
          component: () => import( './views/WelcomeView.vue'),
        },
        {
          path: '/no-pins',
          name: 'no-pins',
          component: () => import( './views/NoPinnedLibraries.vue'),
        },
        {
          path: '/dashboard',
          name: 'dashboard',
          beforeEnter: noLibraryNorPinGuard,
          component: () => import( './views/DashboardView.vue'),
        },
        {
          path: '/settings/users',
          name: 'settings-users',
          beforeEnter: adminGuard,
          component: () => import( './views/SettingsUsers.vue'),
          children: [
            {
              path: '/settings/users/add',
              name: 'settings-users-add',
              component: () => import( './components/dialogs/UserAddDialog.vue'),
            },
          ],
        },
        {
          path: '/settings/server',
          name: 'settings-server',
          beforeEnter: adminGuard,
          component: () => import( './views/SettingsServer.vue'),
        },
        {
          path: '/settings/ui',
          name: 'settings-ui',
          beforeEnter: adminGuard,
          component: () => import( './views/UISettings.vue'),
        },
        {
          path: '/settings/metrics',
          name: 'metrics',
          beforeEnter: adminGuard,
          component: () => import( './views/MetricsView.vue'),
        },
        {
          path: '/settings/announcements',
          name: 'announcements',
          beforeEnter: adminGuard,
          component: () => import( './views/AnnouncementsView.vue'),
        },
        {
          path: '/settings/updates',
          name: 'updates',
          beforeEnter: adminGuard,
          component: () => import( './views/UpdatesView.vue'),
        },
        {
          path: '/media-management/analysis',
          name: 'media-analysis',
          beforeEnter: adminGuard,
          component: () => import( './views/MediaAnalysis.vue'),
        },
        {
          path: '/media-management/missing-posters',
          name: 'missing-posters',
          beforeEnter: adminGuard,
          component: () => import( './views/MissingPosters.vue'),
        },
        {
          path: '/media-management/duplicate-files',
          name: 'duplicate-files',
          beforeEnter: adminGuard,
          component: () => import( './views/DuplicateFiles.vue'),
        },
        {
          path: '/media-management/duplicate-pages/known',
          name: 'settings-duplicate-pages-known',
          beforeEnter: adminGuard,
          component: () => import( './views/DuplicatePagesKnown.vue'),
        },
        {
          path: '/media-management/duplicate-pages/unknown',
          name: 'settings-duplicate-pages-unknown',
          beforeEnter: adminGuard,
          component: () => import( './views/DuplicatePagesUnknown.vue'),
        },
        {
          path: '/history',
          name: 'history',
          beforeEnter: adminGuard,
          component: () => import( './views/HistoryView.vue'),
        },
        {
          path: '/account/me',
          name: 'account-me',
          component: () => import( './views/AccountView.vue'),
        },
        {
          path: '/account/api-keys',
          name: 'account-api-keys',
          component: () => import( './views/ApiKeys.vue'),
        },
        {
          path: '/account/settings-ui',
          name: 'account-settings-ui',
          component: () => import( './views/UIUserSettings.vue'),
        },
        {
          path: '/account/authentication-activity',
          name: 'account-activity',
          component: () => import( './views/SelfAuthenticationActivity.vue'),
        },
        {
          path: '/libraries/:libraryId?',
          name: 'libraries',
          redirect: (route) => ({
            name: getLibraryRoute(route.params.libraryId || LIBRARIES_ALL),
            params: {libraryId: route.params.libraryId || LIBRARIES_ALL},
          }),
        },
        {
          path: '/libraries/:libraryId/recommended',
          name: 'recommended-libraries',
          beforeEnter: noLibraryGuard,
          component: () => import( './views/DashboardView.vue'),
          props: (route) => ({libraryId: route.params.libraryId}),
        },
        {
          path: '/libraries/:libraryId/books',
          name: 'browse-books',
          beforeEnter: noLibraryGuard,
          component: () => import( './views/BrowseBooks.vue'),
          props: (route) => ({libraryId: route.params.libraryId}),
        },
        {
          path: '/libraries/:libraryId/series',
          name: 'browse-libraries',
          beforeEnter: noLibraryGuard,
          component: () => import( './views/BrowseLibraries.vue'),
          props: (route) => ({libraryId: route.params.libraryId}),
        },
        {
          path: '/libraries/:libraryId/collections',
          name: 'browse-collections',
          beforeEnter: noLibraryGuard,
          component: () => import( './views/BrowseCollections.vue'),
          props: (route) => ({libraryId: route.params.libraryId}),
        },
        {
          path: '/libraries/:libraryId/readlists',
          name: 'browse-readlists',
          beforeEnter: noLibraryGuard,
          component: () => import( './views/BrowseReadLists.vue'),
          props: (route) => ({libraryId: route.params.libraryId}),
        },
        {
          path: '/collections/:collectionId',
          name: 'browse-collection',
          component: () => import( './views/BrowseCollection.vue'),
          props: (route) => ({collectionId: route.params.collectionId}),
        },
        {
          path: '/readlists/:readListId',
          name: 'browse-readlist',
          component: () => import( './views/BrowseReadList.vue'),
          props: (route) => ({readListId: route.params.readListId}),
        },
        {
          path: '/series/:seriesId',
          name: 'browse-series',
          component: () => import( './views/BrowseSeries.vue'),
          props: (route) => ({seriesId: route.params.seriesId}),
        },
        {
          path: '/book/:bookId',
          name: 'browse-book',
          component: () => import( './views/BrowseBook.vue'),
          props: (route) => ({bookId: route.params.bookId}),
        },
        {
          path: '/oneshot/:seriesId',
          name: 'browse-oneshot',
          component: () => import( './views/BrowseOneshot.vue'),
          props: (route) => ({seriesId: route.params.seriesId}),
        },
        {
          path: '/search',
          name: 'search',
          component: () => import( './views/SearchView.vue'),
        },
        {
          path: '/import/books',
          name: 'import-books',
          beforeEnter: adminGuard,
          component: () => import( './views/ImportBooks.vue'),
        },
        {
          path: '/import/readlist',
          name: 'import-readlist',
          beforeEnter: adminGuard,
          component: () => import( './views/ImportReadList.vue'),
        },
      ],
    },
    {
      path: '/startup',
      name: 'startup',
      component: () => import( './views/StartupView.vue'),
    },
    {
      path: '/login',
      name: 'login',
      component: () => import( './views/LoginView.vue'),
    },
    {
      path: '/book/:bookId/read',
      name: 'read-book',
      component: () => import( './views/DivinaReader.vue'),
      props: (route) => ({bookId: route.params.bookId}),
    },
    {
      path: '/book/:bookId/read-epub',
      name: 'read-epub',
      component: () => import( './views/EpubReader.vue'),
      props: (route) => ({bookId: route.params.bookId}),
    },
    {
      path: '/:pathMatch(.*)*',
      name: 'notfound',
      component: () => import( './views/PageNotFound.vue'),
    },
  ],
  scrollBehavior(to, from, savedPosition) {
    if (savedPosition) {
      return savedPosition
    } else {
      if (to.name !== from.name) {
        return {left: 0, top: 0}
      }
    }
  },
})

router.beforeEach((to, from, next) => {
  // avoid document.title flickering when changing route
  if (!['read-book', 'read-epub', 'browse-book', 'browse-oneshot', 'browse-series', 'browse-libraries', 'browse-books',
    'recommended-libraries', 'browse-collection', 'browse-collections', 'browse-readlist', 'browse-readlists'].includes(<string>to.name)
  ) {
    document.title = 'Komga'
  }

  if (window.opener !== null &&
    window.name === 'oauth2Login' &&
    to.query.server_redirect === 'Y'
  ) {
    if (!to.query.error) {
      // authentication succeeded, we redirect the parent window so that it can login via cookie
      window.opener.location.href = urls.origin
    } else {
      // authentication failed, we cascade the error message to the parent
      window.opener.location.href = window.location
    }
    // we can close the popup
    window.close()
  }

  if (to.name !== 'startup' && to.name !== 'login' && !lStore.getters.authenticated) {
    const query = Object.assign({}, to.query, {redirect: to.fullPath})
    next({name: 'startup', query: query})
  } else next()
})

export default router
