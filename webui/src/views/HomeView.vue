<template>
  <div class="fill-height">
    <v-app-bar
    >
      <v-badge
        dot
        offset-x="15"
        offset-y="20"
        :model-value="!drawerVisible && !!($store.state.booksToCheck + $store.getters.getUnreadAnnouncementsCount())"
        :color="$store.state.booksToCheck ? 'accent' : 'info'"
        class="ms-n3"
      >
        <v-app-bar-nav-icon @click.stop="toggleDrawer"/>
      </v-badge>

      <search-box class="flex-fill"/>

    </v-app-bar>

    <v-navigation-drawer v-model="drawerVisible" :location="$vuetify.rtl ? 'right' : 'left'">
      <v-list-item @click="$router.push({name: 'home'})" class="pb-2">
        <template #prepend>
          <v-avatar>
            <v-img src="../assets/logo.svg"/>
          </v-avatar>
        </template>

        <div>
          <v-list-item-title class="text-h6">
            Komga
          </v-list-item-title>
        </div>

        <v-tooltip location="left">
          <template v-slot:activator="{ props }">
            <v-progress-linear
              :active="taskCount > 0"
              indeterminate
              absolute
              location="bottom"
              height="5"
              color="secondary"
              v-bind="props"
            />
          </template>
          <div class="mb-2">{{ $tc('common.pending_tasks', taskCount) }}</div>
          <div v-for="taskType in Object.keys(taskCountByType)"
               :key="taskType"
          >{{ taskType }}: {{ taskCountByType[taskType] }}
          </div>
        </v-tooltip>
      </v-list-item>

      <v-divider/>

      <v-slide-x-transition hide-on-leave>
        <reorder-libraries v-if="showReorder" @dismiss="showReorder = false"/>
      </v-slide-x-transition>

      <template v-if="!showReorder">
        <v-list v-model:opened="openedGroups" nav density="compact">
          <v-list-item :to="{name: 'dashboard'}">
            <template #prepend>
              <v-icon>mdi-home</v-icon>
            </template>
            <div>
              <v-list-item-title>{{ $t('navigation.home') }}</v-list-item-title>
            </div>
          </v-list-item>

          <!--   LIBRARIES     -->
          <v-list-item :to="{name:'libraries', params: {libraryId: LIBRARIES_ALL}}">
            <template #prepend>
              <v-icon>mdi-book-multiple</v-icon>
            </template>
            <div>
              <v-list-item-title>{{ $t('navigation.libraries') }}</v-list-item-title>
            </div>
            <template #append>
              <v-list-item-action v-if="isAdmin" class="ma-0">
                <v-btn icon @click.stop.capture.prevent="addLibrary">
                  <v-icon>mdi-plus</v-icon>
                </v-btn>
              </v-list-item-action>
              <v-list-item-action class="ma-0">
                <libraries-actions-menu @reorder="showReorder = true"/>
              </v-list-item-action>
            </template>
          </v-list-item>

          <!--   PINNED LIBRARIES     -->
          <v-list-item v-for="(l, index) in librariesPinned"
                       :key="index"
                       :to="{name:'libraries', params: {libraryId: l.id}}"
          >
            <template #prepend>
            </template>
            <div>
              <v-list-item-title>{{ l.name }}</v-list-item-title>
              <v-list-item-subtitle
                v-if="l.unavailable"
                class="text-error text-caption"
              >{{ $t('common.unavailable') }}
              </v-list-item-subtitle>
            </div>
            <template #append>
              <v-list-item-action class="ma-0" v-if="isAdmin">
                <library-actions-menu :library="l"/>
              </v-list-item-action>
            </template>
          </v-list-item>

          <!--   UNPINNED LIBRARIES     -->
          <v-list-group
            v-if="librariesUnpinned.length > 0"
            value="unpinned"
            subgroup
          >
            <template #activator="{props}">
              <v-list-item v-bind="props" :title="$t('common.more')"/>
            </template>

            <v-list-item v-for="(l, index) in librariesUnpinned"
                         :key="index"
                         :to="{name:'libraries', params: {libraryId: l.id}}"
            >
              <div>
                <v-list-item-title>{{ l.name }}</v-list-item-title>
                <v-list-item-subtitle
                  v-if="l.unavailable"
                  class="text-error text-caption"
                >{{ $t('common.unavailable') }}
                </v-list-item-subtitle>
              </div>
              <template #append>
                <v-list-item-action class="ma-0">
                  <library-actions-menu :library="l"/>
                </v-list-item-action>
              </template>
            </v-list-item>
          </v-list-group>

          <!--   IMPORT     -->
          <v-list-group
            v-if="isAdmin"
            value="import"
          >
            <template #activator="{props}">
              <v-list-item v-bind="props" prepend-icon="mdi-import" :title="$t('book_import.title')"/>
            </template>

            <v-list-item :to="{name: 'import-books'}">
              <v-list-item-title>{{ $t('common.books') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'import-readlist'}">
              <v-list-item-title>{{ $t('common.readlist') }}</v-list-item-title>
            </v-list-item>
          </v-list-group>

          <!--   MEDIA MANAGEMENT     -->
          <v-list-group
            v-if="isAdmin"
            value="media-management"
          >
            <template #activator="{props}">
              <v-list-item v-bind="props" :title="$t('common.media')">
                <template #prepend>
                  <v-badge
                    dot
                    inline
                    :model-value="!!$store.state.booksToCheck"
                    color="accent"
                  >
                    <v-icon>mdi-book-cog</v-icon>
                  </v-badge>
                </template>
              </v-list-item>
            </template>

            <v-list-item :to="{name: 'media-analysis'}">
              <v-badge
                dot
                inline
                :model-value="!!$store.state.booksToCheck"
                color="accent"
              >
                <v-list-item-title>{{ $t('media_analysis.media_analysis') }}</v-list-item-title>
              </v-badge>
            </v-list-item>

            <v-list-item :to="{name: 'missing-posters'}">
              <v-list-item-title>{{ $t('missing_posters.title') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'duplicate-files'}">
              <v-list-item-title>{{ $t('duplicates.title') }}</v-list-item-title>
            </v-list-item>

            <v-list-group
              value="duplicate-pages"
              subgroup
            >
              <template #activator="{props}">
                <v-list-item v-bind="props" :title="$t('duplicate_pages.title')"/>
              </template>

              <v-list-item :to="{name: 'settings-duplicate-pages-known'}">
                <v-list-item-title>{{ $t('duplicate_pages.known') }}</v-list-item-title>
              </v-list-item>

              <v-list-item :to="{name: 'settings-duplicate-pages-unknown'}">
                <v-list-item-title>{{ $t('duplicate_pages.new') }}</v-list-item-title>
              </v-list-item>
            </v-list-group>
          </v-list-group>

          <v-list-item :to="{name: 'history'}" v-if="isAdmin">
            <template #prepend>
              <v-icon>mdi-clock-time-four-outline</v-icon>
            </template>
            <div>
              <v-list-item-title>{{ $t('history.title') }}</v-list-item-title>
            </div>
          </v-list-item>

          <!--   SETTINGS     -->
          <v-list-group
            v-if="isAdmin"
            value="settings"
          >
            <template #activator="{props}">
              <v-list-item v-bind="props" :title="$t('server.tab_title')">
                <template #prepend>
                  <v-badge
                    dot
                    inline
                    :model-value="!!$store.getters.getUnreadAnnouncementsCount()"
                    color="info"
                  >
                    <v-icon>mdi-cog</v-icon>
                  </v-badge>
                </template>
              </v-list-item>
            </template>

            <v-list-item :to="{name: 'settings-users'}">
              <v-list-item-title>{{ $t('users.users') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'settings-server'}">
              <v-list-item-title>{{ $t('common.settings') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'settings-ui'}">
              <v-list-item-title>{{ $t('common.ui') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'metrics'}">
              <v-list-item-title>{{ $t('metrics.title') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'announcements'}">
              <v-badge
                dot
                inline
                :model-value="!!$store.getters.getUnreadAnnouncementsCount()"
                color="info"
              >
                <v-list-item-title>{{ $t('announcements.tab_title') }}</v-list-item-title>
              </v-badge>
            </v-list-item>

            <v-list-item :to="{name: 'updates'}">
              <v-badge
                dot
                inline
                :model-value="$store.getters.isLatestVersion() == 0"
                color="warning"
              >
                <v-list-item-title>{{ $t('server.updates') }}</v-list-item-title>
              </v-badge>
            </v-list-item>
          </v-list-group>

          <!--   ACCOUNT     -->
          <v-list-group
            value="account"
          >
            <template #activator="{props}">
              <v-list-item v-bind="props" prepend-icon="mdi-account" :title="$t('account_settings.my_account')"/>
            </template>

            <v-list-item :to="{name: 'account-me'}">
              <v-list-item-title>{{ $t('account_settings.details') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'account-api-keys'}">
              <v-list-item-title>{{ $t('users.api_keys') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'account-settings-ui'}">
              <v-list-item-title>{{ $t('common.ui') }}</v-list-item-title>
            </v-list-item>

            <v-list-item :to="{name: 'account-activity'}">
              <v-list-item-title>{{ $t('users.authentication_activity') }}</v-list-item-title>
            </v-list-item>
          </v-list-group>

          <v-list-item @click="logout">
            <template #prepend>
              <v-icon>mdi-power</v-icon>
            </template>
            <div>
              <v-list-item-title>{{ $t('navigation.logout') }}</v-list-item-title>
            </div>
          </v-list-item>
        </v-list>

        <v-divider/>

        <v-list density="compact" class="mt-2">
          <v-list-item>
            <template #prepend>
              <v-icon>{{ themeIcon }}</v-icon>
            </template>
            <v-select
              class="py-2" density="compact"
              v-model="theme"
              :items="themes"
              :label="$t('home.theme')"
            ></v-select>
          </v-list-item>

          <v-list-item>
            <template #prepend>
              <v-icon>mdi-translate</v-icon>
            </template>
            <v-select density="compact"
              class="py-2"
              v-model="locale"
              :items="locales"
              :label="$t('home.translation')"
            >
            </v-select>
          </v-list-item>
        </v-list>

        <v-spacer/>
      </template>

      <template v-slot:append>
        <div v-if="isAdmin && !$_.isEmpty($store.state.actuatorInfo)"
             class="pa-2 pb-6 text-caption"
        >
          <v-badge
            dot
            :model-value="$store.getters.isLatestVersion() == 0"
            color="warning"
          >
            <router-link :to="{name: 'updates'}" class="link-none">
              v{{ $store.state.actuatorInfo.build.version }}-{{ $store.state.actuatorInfo.git.branch }}
            </router-link>
          </v-badge>
        </div>
      </template>
    </v-navigation-drawer>

    <v-main class="fill-height">
      <reusable-dialogs/>
      <toaster-notification/>
      <router-view/>
    </v-main>
  </div>
</template>

<script lang="ts">
import ReusableDialogs from '@/components/ReusableDialogs.vue'
import LibraryActionsMenu from '@/components/menus/LibraryActionsMenu.vue'
import SearchBox from '@/components/SearchBox.vue'
import {Theme} from '@/types/themes'
import {defineComponent} from 'vue'
import {LIBRARIES_ALL} from '@/types/library'
import ToasterNotification from '@/components/ToasterNotification.vue'
import {MediaStatus} from '@/types/enum-books'
import {LibraryDto} from '@/types/komga-libraries'
import {BookSearch, SearchConditionAnyOfBook, SearchConditionMediaStatus, SearchOperatorIs} from '@/types/komga-search'
import LibrariesActionsMenu from '@/components/menus/LibrariesActionsMenu.vue'
import ReorderLibraries from '@/components/ReorderLibraries.vue'

export default defineComponent({
  name: 'HomeView',
  components: {
    ReorderLibraries,
    LibrariesActionsMenu,
    ToasterNotification,
    LibraryActionsMenu,
    SearchBox,
    ReusableDialogs,
  },
  data: function () {
    return {
      LIBRARIES_ALL,
      drawerVisible: this.$vuetify.breakpoint.lgAndUp,
      locales: this.$i18n.availableLocales.map((x: any) => ({text: this.$t('common.locale_name', x), value: x})),
      openedGroups: [] as string[],
      showReorder: false,
    }
  },
  async created() {
    if (this.isAdmin) {
      this.$actuator.getInfo()
        .then(x => this.$store.commit('setActuatorInfo', x))
      this.$komgaBooks.getBooksList({
        condition: new SearchConditionAnyOfBook([
          new SearchConditionMediaStatus(new SearchOperatorIs(MediaStatus.ERROR)),
          new SearchConditionMediaStatus(new SearchOperatorIs(MediaStatus.UNSUPPORTED)),
        ]),
      } as BookSearch, {size: 1} as PageRequest)
        .then(x => this.$store.commit('setBooksToCheck', x.totalElements))
      this.$komgaAnnouncements.getAnnouncements()
        .then(x => this.$store.commit('setAnnouncements', x))
      this.$komgaReleases.getReleases()
        .then(x => this.$store.commit('setReleases', x))
    }
    this.checkRoute(this.$route)
  },
  watch: {
    $route(to, from) {
      this.checkRoute(to)
    },
  },
  computed: {
    taskCount(): number {
      return this.$store.state.komgaSse.taskCount
    },
    taskCountByType(): { [key: string]: number } {
      return this.$store.state.komgaSse.taskCountByType
    },
    libraries(): LibraryDto[] {
      return this.$store.getters.getLibraries
    },
    librariesPinned(): LibraryDto[] {
      return this.$store.getters.getLibrariesPinned
    },
    librariesUnpinned(): LibraryDto[] {
      return this.$store.getters.getLibrariesUnpinned
    },
    isAdmin(): boolean {
      return this.$store.getters.meAdmin
    },
    themes(): object[] {
      return [
        {text: this.$t(Theme.LIGHT), value: Theme.LIGHT},
        {text: this.$t(Theme.DARK), value: Theme.DARK},
        {text: this.$t(Theme.SYSTEM), value: Theme.SYSTEM},
      ]
    },
    themeIcon(): string {
      switch (this.theme) {
        case Theme.LIGHT:
          return 'mdi-brightness-7'
        case Theme.DARK:
          return 'mdi-brightness-3'
        case Theme.SYSTEM:
          return 'mdi-brightness-auto'
      }
      return ''
    },

    theme: {
      get: function (): Theme {
        return this.$store.state.persistedState.theme
      },
      set: function (theme: Theme): void {
        if (Object.values(Theme).includes(theme)) {
          this.$store.commit('setTheme', theme)
        }
      },
    },
    locale: {
      get: function (): string {
        return this.$i18n.locale
      },
      set: function (locale: string): void {
        if (this.$i18n.availableLocales.includes(locale)) {
          this.$store.commit('setLocale', locale)
        }
      },
    },
  },
  methods: {
    checkRoute(to) {
      const openedGroups: string[] = []
      if (to.path.includes('/settings/')) openedGroups.push('settings')
      if (to.path.includes('/media-management/')) openedGroups.push('media-management')
      if (to.path.includes('/duplicate-pages/')) openedGroups.push('media-management', 'duplicate-pages')
      if (to.path.includes('/import/')) openedGroups.push('import')
      if (to.path.includes('/account/')) openedGroups.push('account')
      if (this.librariesUnpinned.some(it => it.id === to.params.libraryId)) openedGroups.push('unpinned')
      this.openedGroups = [...new Set(openedGroups)]
    },
    toggleDrawer() {
      this.drawerVisible = !this.drawerVisible
    },
    logout() {
      this.$store.dispatch('logout')
      this.$router.push({name: 'login', query: {'logout': true}})
    },
    addLibrary() {
      this.$store.dispatch('dialogAddLibrary')
    },
  },
})
</script>
