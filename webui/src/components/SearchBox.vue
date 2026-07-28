<template>
  <div>
    <v-autocomplete
      v-model="selectedItem"
      :placeholder="$t('search.search')"
      :no-data-text="$t('searchbox.no_results')"
      :loading="loading"
      :items="results"
      :hide-no-data="!showResults"
      clearable variant="solo"
      hide-details
      no-filter
      return-object
      prepend-inner-icon="mdi-magnify"
      append-icon=""
      item-title="id"
      item-value="id"
      auto-select-first
      v-model:search="search"
      :menu-props="{maxHeight: $vuetify.breakpoint.height * .9, minWidth: $vuetify.breakpoint.mdAndUp ? $vuetify.breakpoint.width * .4 : $vuetify.breakpoint.width * .8}"
      @keydown.esc="clear"
      @blur="clear"
      ref="searchbox"
    >
      <template v-slot:selection>
      </template>

      <template #item="{props, item}">
        <v-list-item v-bind="props">
          <template v-if="item.raw.type === 'search'">
            <div>{{ $t('searchbox.search_all') }}</div>
          </template>

          <template v-if="item.raw.type === 'series'">
            <div class="d-flex align-center">
              <v-img :src="seriesThumbnailUrl(item.raw.id)"
                 height="50"
                 max-width="35"
                 class="my-1 mx-3"
          >
                <span v-if="item.raw.booksUnreadCount !== 0"
                      class="text-white pa-0 px-1 text-caption"
                      :style="{background: 'orange', position: 'absolute', right: 0}"
                >
                  {{ item.raw.booksUnreadCount }}
                </span>
              </v-img>
              <div>
                <v-list-item-title>{{ item.raw.metadata.title }}</v-list-item-title>
                <v-list-item-subtitle>{{
                    $t('searchbox.in_library', {library: getLibraryName(item.raw)})
                  }}
                </v-list-item-subtitle>
                <v-list-item-subtitle v-if="item.raw.booksMetadata.releaseDate">{{
                    new Intl.DateTimeFormat($i18n.locale, {
                      year: 'numeric',
                      timeZone: 'UTC'
                    }).format(new Date(item.raw.booksMetadata.releaseDate))
                  }}
                </v-list-item-subtitle>
              </div>
            </div>
          </template>

          <template v-if="item.raw.type === 'book'">
            <div class="d-flex align-center">
              <v-img :src="bookThumbnailUrl(item.raw.id)"
                 height="50"
                 max-width="35"
                 class="my-1 mx-3"
          >
                <div class="unread" v-if="isUnread(item.raw)"/>
              </v-img>

              <div>
                <v-list-item-title>{{ item.raw.metadata.title }}</v-list-item-title>
                <v-list-item-subtitle v-if="!item.raw.oneshot">{{ item.raw.seriesTitle }} - {{
                    item.raw.metadata.number
                  }}
                </v-list-item-subtitle>
                <v-list-item-subtitle>{{
                    $t('searchbox.in_library', {library: getLibraryName(item.raw)})
                  }}
                </v-list-item-subtitle>
              </div>
            </div>
          </template>

          <template v-if="item.raw.type === 'collection'">
            <div class="d-flex align-center">
              <v-img :src="collectionThumbnailUrl(item.raw.id)"
                 height="50"
                 max-width="35"
                 class="my-1 mx-3"
              />
              <v-list-item-title>{{ item.raw.name }}</v-list-item-title>
            </div>
          </template>

          <template v-if="item.raw.type === 'readlist'">
            <div class="d-flex align-center">
              <v-img :src="readListThumbnailUrl(item.raw.id)"
                 height="50"
                 max-width="35"
                 class="my-1 mx-3"
              />
              <v-list-item-title>{{ item.raw.name }}</v-list-item-title>
            </div>
          </template>
        </v-list-item>
      </template>
    </v-autocomplete>
  </div>
</template>

<script lang="ts">
import {bookThumbnailUrl, collectionThumbnailUrl, readListThumbnailUrl, seriesThumbnailUrl} from '@/functions/urls'
import {debounce} from 'lodash'
import {defineComponent} from 'vue'
import {BookDto} from '@/types/komga-books'
import {SeriesDto} from '@/types/komga-series'
import {getReadProgress} from '@/functions/book-progress'
import {ReadStatus} from '@/types/enum-books'
import {ReadListDto} from '@/types/komga-readlists'
import {
  BookSearch,
  SearchConditionOneShot,
  SearchOperatorIsFalse,
  SeriesSearch,
} from '@/types/komga-search'

export default defineComponent({
  name: 'SearchBox',
  data: function () {
    return {
      selectedItem: null as unknown as any,
      search: null,
      showResults: false,
      loading: false,
      series: [] as SeriesDto[],
      books: [] as BookDto[],
      collections: [] as CollectionDto[],
      readLists: [] as ReadListDto[],
      pageSize: 10,
    }
  },
  watch: {
    selectedItem(val, old) {
      if (val && val.hasOwnProperty('type')) {
        this.$nextTick(() => {
          this.selectedItem = undefined
        })

        if (val.type === 'series') this.$router.push({name: 'browse-series', params: {seriesId: val.id}})
        else if (val.type === 'book' && val.oneshot) this.$router.push({
          name: 'browse-oneshot',
          params: {seriesId: val.seriesId},
        })
        else if (val.type === 'book') this.$router.push({name: 'browse-book', params: {bookId: val.id}})
        else if (val.type === 'collection') this.$router.push({
          name: 'browse-collection',
          params: {collectionId: val.id},
        })
        else if (val.type === 'readlist') this.$router.push({name: 'browse-readlist', params: {readListId: val.id}})
        else if (val.type === 'search') this.searchDetails()

        //@ts-ignore
        this.$refs.searchbox.blur()
      }
    },
    search(val) {
      this.searchItems(val)
    },
    showResults(val) {
      !val && this.clear()
    },
  },
  computed: {
    results(): object[] {
      const results = []
      if (this.search) {
        results.push({type: 'search', id: '__search__'})
        if (this.series.length > 0) {
          results.push({type: 'subheader', id: this.$tc('common.series', 2).toString().toUpperCase()})
          results.push(...this.series.map(o => ({...o, type: 'series'})))
        }
        if (this.books.length > 0) {
          results.push({type: 'subheader', id: this.$t('common.books').toString().toUpperCase()})
          results.push(...this.books.map(o => ({...o, type: 'book'})))
        }
        if (this.collections.length > 0) {
          results.push({type: 'subheader', id: this.$t('common.collections').toString().toUpperCase()})
          results.push(...this.collections.map(o => ({...o, type: 'collection'})))
        }
        if (this.readLists.length > 0) {
          results.push({type: 'subheader', id: this.$t('common.readlists').toString().toUpperCase()})
          results.push(...this.readLists.map(o => ({...o, type: 'readlist'})))
        }
      }
      return results
    },
  },
  methods: {
    getLibraryName(item: BookDto | SeriesDto): string {
      return this.$store.getters.getLibraryById(item.libraryId).name
    },
    searchItems: debounce(async function (this: any, query: string) {
      if (query) {
        this.loading = true
        this.series = (await this.$komgaSeries.getSeriesList({
          fullTextSearch: query,
          condition: new SearchConditionOneShot(new SearchOperatorIsFalse()),
        } as SeriesSearch, {size: this.pageSize})).content
        this.books = (await this.$komgaBooks.getBooksList({
          fullTextSearch: query,
        } as BookSearch, {size: this.pageSize})).content
        this.collections = (await this.$komgaCollections.getCollections(undefined, {size: this.pageSize}, query)).content
        this.readLists = (await this.$komgaReadLists.getReadLists(undefined, {size: this.pageSize}, query)).content
        this.showResults = true
        this.loading = false
      } else {
        this.clear()
      }
    }, 500),
    clear() {
      this.search = null
      this.showResults = false
      this.series = []
      this.books = []
      this.collections = []
      this.readLists = []
    },
    searchDetails() {
      const s = this.search
      this.clear()
      this.$router.push({name: 'search', query: {q: s}}).catch(e => {
      })
    },
    isUnread(book: BookDto): boolean {
      return getReadProgress(book) === ReadStatus.UNREAD
    },
    seriesThumbnailUrl(seriesId: string): string {
      return seriesThumbnailUrl(seriesId)
    },
    bookThumbnailUrl(bookId: string): string {
      return bookThumbnailUrl(bookId)
    },
    collectionThumbnailUrl(collectionId: string): string {
      return collectionThumbnailUrl(collectionId)
    },
    readListThumbnailUrl(readListId: string): string {
      return readListThumbnailUrl(readListId)
    },
  },
})
</script>

<style scoped>
.unread {
  border-left: 15px solid transparent;
  border-right: 15px solid orange;
  border-bottom: 15px solid transparent;
  height: 0;
  width: 0;
  position: absolute;
  right: 0;
  z-index: 2;
}
</style>
