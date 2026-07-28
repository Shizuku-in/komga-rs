<template>
  <v-list density="compact">
    <div v-for="(f, key) in filtersOptions"
         :key="key"
    >
      <v-list-subheader v-if="f.name">{{ f.name }}</v-list-subheader>
      <v-list-item v-for="v in f.values"
                   :key="JSON.stringify(v.value)"
                   @click.stop="click(key, v.value, v.nValue)"
      >
        <template #prepend>
          <v-icon v-if="key in filtersActive && includes(filtersActive[key], v.nValue)" color="secondary">
            mdi-minus-box
          </v-icon>
          <v-icon v-else-if="key in filtersActive && includes(filtersActive[key], v.value)" color="secondary">
            mdi-checkbox-marked
          </v-icon>
          <v-icon v-else>
            mdi-checkbox-blank-outline
          </v-icon>
        </template>
        <v-list-item-title>{{ v.name }}</v-list-item-title>
      </v-list-item>
    </div>
  </v-list>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {FiltersActive, FiltersOptions} from '@/types/filter'

export default defineComponent({
  name: 'FilterList',
  props: {
    filtersOptions: {
      type: Object as () => FiltersOptions,
      required: true,
    },
    filtersActive: {
      type: Object as () => FiltersActive,
      required: true,
    },
  },
  methods: {
    includes(array: any[], value: any): boolean {
      return this.$_.isObject(value) ? this.$_.some(array, value) : this.$_.includes(array, value)
    },
    click(key: string, value: any, nValue?: any) {
      let r = this.$_.cloneDeep(this.filtersActive)
      if (!(key in r)) r[key] = []

      const pull = this.$_.isObject(value) ? this.$_.remove : this.$_.pull
      const includes = this.$_.isObject(value) ? this.$_.some : this.$_.includes

      if (nValue && includes(r[key], nValue))
        pull(r[key], nValue)
      else if (includes(r[key], value)) {
        pull(r[key], value)
        if (nValue)
          r[key].push(nValue)
      } else
        r[key].push(value)

      this.$emit('update:filtersActive', r)
    },
  },
})
</script>

<style scoped>

</style>
