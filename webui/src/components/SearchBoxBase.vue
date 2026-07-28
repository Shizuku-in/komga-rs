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
      no-filter density="compact"
      append-icon=""
      auto-select-first
      v-model:search="search"
      @keydown.esc="clear"
      ref="searchbox"
    >
      <template v-slot:selection>
      </template>

      <template #item="{props, item}">
        <v-list-item v-bind="props">
          <slot name="item" :item="item.raw"/>
        </v-list-item>
      </template>
    </v-autocomplete>
  </div>
</template>

<script lang="ts">
import {debounce} from 'lodash'
import {defineComponent, PropType} from 'vue'

export default defineComponent({
  name: 'SearchBoxBase',
  data: function () {
    return {
      selectedItem: null as unknown as any,
      search: null,
      showResults: false,
      loading: false,
      results: [],
    }
  },
  watch: {
    selectedItem(val, old) {
      if (val) {
        this.$nextTick(() => {
          this.selectedItem = undefined
          this.clear()
        })

        this.$emit('selected', val)

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
  props: {
    searchFunction: {
      type: Function as PropType<Function>,
      required: true,
    },
  },
  methods: {
    searchItems: debounce(async function (this: any, query: string) {
      if (query) {
        this.loading = true

        this.results = await this.searchFunction.apply(this, [query])

        this.showResults = true
        this.loading = false
      } else {
        this.clear()
      }
    }, 500),
    clear() {
      this.search = null
      this.results = []
      this.showResults = false
    },
  },
})
</script>
